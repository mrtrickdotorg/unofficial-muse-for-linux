//! The web UI slice of the port.
//!
//! Pages render the way the migration will render every page: a WebKitGTK
//! view (the engine the feed widgets and the space preview already use
//! in-window), a React app from `apps/muse/ui` built to a single inlined HTML
//! file, and the Rust core as the only source of data. Library is the
//! reference page; Goals is the second, on the same contract.
//!
//! Two halves meet here:
//!
//! - **Data in.** The boot payload is inlined into the document so the page
//!   paints immediately from what the native page last read. It names its
//!   page, so one bundle serves every window.
//! - **Commands out.** A `UserContentManager` script-message handler named
//!   `muse` receives `{"id","cmd"}` from the page and answers through
//!   `window.museReply(id, reply)`.
//!
//! Commands say what they do. `<page>.snapshot` answers from the held rows, so
//! a page opens without a wire trip; `<page>.refresh` reads the service on a
//! worker thread and answers when that listing lands. A refresh that only
//! re-read the cache would be a lie in the button's own label.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use muse_account::{AccountClient, ArtifactSummary};

use crate::pages::{
    HeldChat, HeldFeed, HeldGoals, HeldIdeas, HeldLocalHost, HeldSearch, HeldSettings, HeldTasks,
    HeldThreads,
};

/// How long the main loop sleeps between looks at a worker's answer. A wire
/// read takes hundreds of milliseconds at best and has taken 27 s against
/// this service; ten looks a second is finer than a person can tell apart,
/// while an idle handler -- which runs whenever the loop has nothing else to
/// do -- would spin a core for the whole wait.
const ANSWER_POLL: Duration = Duration::from_millis(100);

/// Whether a WebView can be constructed at all (helpers staged and loading).
pub(crate) fn available() -> bool {
    crate::widget::widget_available()
}

/// The document: the built React bundle with this page's boot payload in it.
/// Inlined rather than fetched so the page renders in one load with no local
/// server and no custom URI scheme to audit.
fn document(boot: &serde_json::Value) -> String {
    // A title containing `</script>` must not be able to close the data
    // block; JSON's own escape preserves the string's meaning.
    let payload = boot.to_string().replace("</", "<\\/");
    include_str!("../ui/built/app.html").replace("/*__BOOT__*/", &payload)
}

fn library_document(artifacts: &[ArtifactSummary], total: u64) -> String {
    document(&serde_json::json!({
        "page": "library",
        "artifacts": artifacts,
        "total": total,
    }))
}

fn goals_document(held: &HeldGoals) -> String {
    document(&serde_json::json!({
        "page": "goals",
        "goals": held.goals,
        "goals_total": held.goals_total,
        "tracking": held.tracking,
        "tracking_total": held.tracking_total,
        "tracking_error": held.tracking_error,
    }))
}

/// The Tasks rows, shaped once for both the boot payload and the reply.
fn tasks_payload(held: &HeldTasks) -> serde_json::Value {
    serde_json::json!({
        "schedules": held.schedules,
        "runs": held.runs,
        "local": held.local,
        "next_run": held.next_run,
        "loaded": held.loaded,
        "error": held.error,
    })
}

/// The Tasks page as the shell window paints it from a `show` line: the held
/// rows and the page's own name, with no document around them. A notification
/// click opens this page through [`crate::web_shell::show_page`].
pub(crate) fn tasks_boot() -> serde_json::Value {
    let mut boot = tasks_payload(&crate::pages::tasks_snapshot());
    boot["page"] = serde_json::json!("tasks");
    boot
}

fn tasks_document(held: &HeldTasks) -> String {
    let mut boot = tasks_payload(held);
    boot["page"] = serde_json::json!("tasks");
    document(&boot)
}

fn tasks_reply(held: &HeldTasks) -> serde_json::Value {
    let mut reply = tasks_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// The Ideas rows, shaped once for both the boot payload and the reply.
fn ideas_payload(held: &HeldIdeas) -> serde_json::Value {
    serde_json::json!({
        "ideas": held.ideas,
        "total": held.total,
        "loaded": held.loaded,
        "error": held.error,
    })
}

fn ideas_document(held: &HeldIdeas) -> String {
    let mut boot = ideas_payload(held);
    boot["page"] = serde_json::json!("ideas");
    document(&boot)
}

fn ideas_reply(held: &HeldIdeas) -> serde_json::Value {
    let mut reply = ideas_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// The Threads rows, shaped once for both the boot payload and the reply.
fn threads_payload(held: &HeldThreads) -> serde_json::Value {
    serde_json::json!({
        "threads": held.threads,
        "total": held.total,
        "loaded": held.loaded,
        "error": held.error,
        // The row whose Delete press is armed, if any: the app owns the
        // arming, so the page paints it from here instead of keeping a copy.
        "armed": crate::pages::thread_delete_armed(),
    })
}

fn threads_document(held: &HeldThreads) -> String {
    let mut boot = threads_payload(held);
    boot["page"] = serde_json::json!("threads");
    document(&boot)
}

fn threads_reply(held: &HeldThreads) -> serde_json::Value {
    let mut reply = threads_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// The Feed cards, shaped once for both the boot payload and the reply.
fn feed_payload(held: &HeldFeed) -> serde_json::Value {
    serde_json::json!({
        "cards": held.cards,
        "prompt": held.prompt,
        "status": held.status,
        "total": held.total,
        "loaded": held.loaded,
        "error": held.error,
    })
}

fn feed_document(held: &HeldFeed) -> String {
    let mut boot = feed_payload(held);
    boot["page"] = serde_json::json!("feed");
    document(&boot)
}

fn feed_reply(held: &HeldFeed) -> serde_json::Value {
    let mut reply = feed_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// Standard base64, so a cached hero can travel to the page as a data URI:
/// a document loaded from `about:blank` cannot read `file://` images, and
/// inlining every hero in the boot payload would carry megabytes.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Read a hero's cached bytes on a worker thread, as a data URI. Cache-only
/// on the wire -- the native fill owns the download -- and off the main loop
/// too: even a local read of a capped thumbnail is file I/O the UI thread
/// should not wait on while a card asks for it.
fn feed_hero(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    let path = crate::images::fetch_hero_cached(url)?;
    let bytes = std::fs::read(&path).ok()?;
    let mime = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "image/jpeg",
    };
    Some(format!("data:{mime};base64,{}", base64(&bytes)))
}

/// The Search page as both the boot payload and a reply carry it.
fn search_payload(held: &HeldSearch) -> serde_json::Value {
    serde_json::json!({
        "query": held.query,
        "rows": held.rows,
        "total": held.total,
        "loaded": held.loaded,
        "error": held.error,
    })
}

fn search_document(held: &HeldSearch) -> String {
    let mut boot = search_payload(held);
    boot["page"] = serde_json::json!("search");
    document(&boot)
}

fn search_reply(held: &HeldSearch) -> serde_json::Value {
    let mut reply = search_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// The Local host page as both the boot payload and a reply carry it. The
/// host's read model is projected natively; nothing is read here.
fn localhost_payload(held: &HeldLocalHost) -> serde_json::Value {
    serde_json::to_value(held).expect("muse webui: the local host payload always serializes")
}

fn localhost_document(held: &HeldLocalHost) -> String {
    let mut boot = localhost_payload(held);
    boot["page"] = serde_json::json!("localhost");
    document(&boot)
}

fn signin_payload() -> serde_json::Value {
    serde_json::to_value(crate::pages::signin_snapshot())
        .expect("muse webui: the sign-in payload always serializes")
}

fn signin_document() -> String {
    document(&signin_boot())
}

/// The sign-in page's boot payload, named: the switch line the window paints
/// when the app has returned to sign-in (see `web_shell::show_signin`).
pub(crate) fn signin_boot() -> serde_json::Value {
    let mut boot = signin_payload();
    boot["page"] = serde_json::json!("signin");
    boot
}

fn signin_reply() -> serde_json::Value {
    let mut reply = signin_payload();
    reply["ok"] = serde_json::json!(true);
    reply
}

fn first_run_payload() -> serde_json::Value {
    serde_json::to_value(crate::pages::first_run_snapshot())
        .expect("muse webui: the first-run payload always serializes")
}

fn first_run_document() -> String {
    let mut boot = first_run_payload();
    boot["page"] = serde_json::json!("firstrun");
    document(&boot)
}

fn first_run_reply() -> serde_json::Value {
    let mut reply = first_run_payload();
    reply["ok"] = serde_json::json!(true);
    reply
}

fn localhost_reply(held: &HeldLocalHost) -> serde_json::Value {
    let mut reply = localhost_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// The shell page: the rail and its page list, plus the account and build
/// stamp the native drawer prints. Each page inside the shell asks its own
/// snapshot, so the shell carries no page data of its own.
fn shell_payload(routes: Option<&WebRoutes>) -> serde_json::Value {
    let chat = crate::pages::chat_snapshot();
    serde_json::json!({
        "pages": crate::pages::shell_pages(),
        // Empty until the app has a name for the session: a page prints its own
        // "Sign in" for that, and the app pushes a fresh payload when the name
        // arrives rather than asking the drawer to poll for it.
        "account": chat.account,
        "build": match crate::update::local_sha() {
            Some(sha) => format!("build {}", sha.get(..7).unwrap_or(sha)),
            None => "dev build".to_owned(),
        },
        // The drawer's History is the panel's listing, so it arrives only with
        // the routes that can read it. A window opened without them shows the
        // section unread rather than empty.
        "history": match routes {
            Some(routes) => match (routes.history)() {
                Ok(rows) => serde_json::json!(rows),
                Err(_) => serde_json::Value::Null,
            },
            None => serde_json::Value::Null,
        },
    })
}

/// The shell's boot payload as the app holds it now: what [`shell_document`]
/// starts from, and what the app pushes the window when its own read fills the
/// drawer (see `web_shell::account_named`).
pub(crate) fn shell_boot(routes: Option<&WebRoutes>) -> serde_json::Value {
    let mut boot = shell_payload(routes);
    boot["page"] = serde_json::json!("shell");
    boot
}

pub(crate) fn shell_document(routes: Option<&WebRoutes>) -> String {
    document(&shell_boot(routes))
}

fn shell_reply(routes: Option<&WebRoutes>) -> serde_json::Value {
    let mut reply = shell_payload(routes);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// The Settings page as both the boot payload and a reply carry it. Nothing
/// here is read on this side: the link, the updater's answer and the policy
/// are the native page's own held state, projected once.
fn settings_payload(held: &HeldSettings) -> serde_json::Value {
    serde_json::to_value(held).expect("muse webui: the settings payload always serializes")
}

fn settings_document(held: &HeldSettings) -> String {
    let mut boot = settings_payload(held);
    boot["page"] = serde_json::json!("settings");
    document(&boot)
}

fn settings_reply(held: &HeldSettings) -> serde_json::Value {
    let mut reply = settings_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// The Chat surface, shaped once for both the boot payload and the reply.
/// Every field is the native window's own: the thread title, the rows the
/// transcript last built, the composer's status line.
fn chat_payload(held: &HeldChat) -> serde_json::Value {
    serde_json::json!({
        "thread": held.thread,
        "title": held.title,
        "account": held.account,
        "status": held.status,
        "sending": held.sending,
        // The pictures staged in the app's own composer, the open thread's
        // unread count from the listing, and the pending approvals the panel
        // last painted: three things the page cannot read on its own.
        "staged": held.staged,
        "unread": held.unread,
        "approvals": held.approvals,
        // The turn menu's press and its own label: the page prints the app's
        // words and cannot show "Delete" beside an app waiting to confirm.
        "turn": crate::pages::turn_view(),
        // The draft both surfaces share, and the mic's state: the page types
        // into the native window's own draft instead of keeping a second one.
        "composer": crate::pages::composer_view(),
        "rows": held.rows,
        "loaded": held.loaded,
        "error": held.error,
    })
}

fn chat_document(held: &HeldChat) -> String {
    let mut boot = chat_payload(held);
    boot["page"] = serde_json::json!("chat");
    document(&boot)
}

fn chat_reply(held: &HeldChat) -> serde_json::Value {
    let mut reply = chat_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// What a fresh wire read produced off the main loop.
pub(crate) enum Fetched {
    Rows(Vec<ArtifactSummary>, u64),
    Goals(HeldGoals),
    Tasks(HeldTasks),
    /// One schedule's delete answer, and the Tasks rows read again after it:
    /// acceptance is the service's word, the rows are the proof.
    TaskDelete {
        job: String,
        accepted: bool,
        error: Option<String>,
        held: Option<HeldTasks>,
        reread_error: Option<String>,
    },
    Ideas(HeldIdeas),
    Threads(HeldThreads),
    /// One conversation's rename, pin or archive, and the listing read again
    /// after it: `accepted` is the service's word, the rows are the proof.
    /// `rows` is None when the verb was refused (nothing changed, so there was
    /// nothing to read back).
    ThreadOp {
        id: String,
        verb: String,
        accepted: bool,
        error: Option<String>,
        rows: Option<HeldThreads>,
        threads: Vec<muse_account::Thread>,
        reread_error: Option<String>,
    },
    Feed(HeldFeed),
    /// A cached hero's data URI, or None when nothing is cached.
    Hero(Option<String>),
    /// One space's unshare answer, and the Library listing read again after
    /// it: acceptance is the service's, the change is whatever that read
    /// says. `listing` is None when the re-read failed, so the reply can say
    /// the change is not proven instead of painting the held rows as new.
    Unshare {
        slug: String,
        accepted: bool,
        error: Option<String>,
        listing: Option<(Vec<ArtifactSummary>, u64)>,
        reread_error: Option<String>,
    },
    /// One space's export, for the preview window the web page's Open asks
    /// for. The app draws that window, so the page is handed only the facts
    /// about what was drawn.
    SpaceExport {
        name: String,
        exported: Result<muse_account::SpaceExportAnswer, String>,
    },
    /// One space's delete answer, and the Library listing read again after
    /// it, for the same reason the unshare carries one: acceptance is not the
    /// change, the read is.
    SpaceDelete {
        slug: String,
        accepted: bool,
        error: Option<String>,
        listing: Option<(Vec<ArtifactSummary>, u64)>,
        reread_error: Option<String>,
    },
    Search(HeldSearch),
    Failed(String),
}

/// A command's answer. Cache reads answer at once; a wire read answers when
/// its thread lands, so the window keeps painting during a slow listing.
pub(crate) enum Answer {
    Now(serde_json::Value),
    Later(Receiver<Fetched>),
}

/// Read the Library from the service on a worker thread. No client means no
/// account link in this app, which the page reads as a failure rather than
/// as an empty library.
fn fresh_listing(client: Option<AccountClient>) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let fetched = match client {
            None => Fetched::Failed("no account link in this app".into()),
            Some(client) => match client.artifacts() {
                Ok(listing) => Fetched::Rows(listing.artifacts, listing.total_count),
                Err(error) => Fetched::Failed(error.to_string()),
            },
        };
        let _ = sender.send(fetched);
    });
    receiver
}

/// Read the Tasks page from the service on a worker thread. The rows keep
/// the native page's wording; the local reminders are local state and are
/// merged in on the main loop.
fn fresh_tasks(client: Option<AccountClient>) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let fetched = match client {
            None => Fetched::Failed("no account link in this app".into()),
            Some(client) => match client.tasks() {
                Ok(snapshot) => Fetched::Tasks(crate::pages::held_tasks_from(&snapshot)),
                Err(error) => Fetched::Failed(error.to_string()),
            },
        };
        let _ = sender.send(fetched);
    });
    receiver
}

/// Delete one schedule on a worker thread, then read the Tasks listing again:
/// the delete's acceptance and the rows as they now are travel together, so a
/// reply can say the change is not proven rather than painting a stale row.
fn fresh_task_delete(client: Option<AccountClient>, job: String) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(client) = client else {
            let _ = sender.send(Fetched::Failed("no account link in this app".into()));
            return;
        };
        let (accepted, error) = match client.delete_task(&job) {
            Ok(answer) => (answer.accepted, None),
            Err(failure) => (false, Some(failure.to_string())),
        };
        let (held, reread_error) = match client.tasks() {
            Ok(snapshot) => (Some(crate::pages::held_tasks_from(&snapshot)), None),
            Err(failure) => (None, Some(failure.to_string())),
        };
        let _ = sender.send(Fetched::TaskDelete {
            job,
            accepted,
            error,
            held,
            reread_error,
        });
    });
    receiver
}

/// The title a rename may carry: the service caps its own, and a page that
/// pasted a paragraph is refused here rather than on the wire.
const MAX_THREAD_TITLE: usize = 200;

/// One listing read after a change, shared by every conversation write: the
/// rows the page paints, the raw threads the panel adopts, and the read's own
/// failure when it had one -- acceptance is never mistaken for the change.
type ThreadReadback = (
    Option<HeldThreads>,
    Vec<muse_account::Thread>,
    Option<String>,
);

fn reread_threads(client: &AccountClient) -> ThreadReadback {
    match client.connect() {
        Ok(snapshot) => (
            Some(crate::pages::held_threads_from(
                &snapshot.threads,
                &crate::app::local_today(),
            )),
            snapshot.threads,
            None,
        ),
        Err(failure) => (None, Vec::new(), Some(failure.to_string())),
    }
}

/// Clear one conversation's unread badge on a worker thread: one `mark_seen`
/// in the recorded consumer's shape, then the listing read again. The write's
/// answer is acceptance; the rows are the proof there is.
fn fresh_thread_seen(client: Option<AccountClient>, id: String) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(client) = client else {
            let _ = sender.send(Fetched::Failed("no account link in this app".into()));
            return;
        };
        let (accepted, error) = match client.mark_seen(&id) {
            Ok(answer) => (answer.accepted, None),
            Err(failure) => (false, Some(failure.to_string())),
        };
        let (rows, threads, reread_error) = if accepted {
            reread_threads(&client)
        } else {
            (None, Vec::new(), None)
        };
        let _ = sender.send(Fetched::ThreadOp {
            id,
            verb: "seen".into(),
            accepted,
            error,
            rows,
            threads,
            reread_error,
        });
    });
    receiver
}

/// Rename, pin or archive one conversation on a worker thread: the verb, then
/// the listing read again. Acceptance is the service's; the rows are what the
/// page paints, and a refused verb changes nothing at all.
fn fresh_thread_op(
    client: Option<AccountClient>,
    id: String,
    verb: String,
    title: Option<String>,
) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(client) = client else {
            let _ = sender.send(Fetched::Failed("no account link in this app".into()));
            return;
        };
        let (accepted, error) = match client.session_op(&id, &verb, title.as_deref()) {
            Ok(answer) => (answer.accepted, None),
            Err(failure) => (false, Some(failure.to_string())),
        };
        let (rows, threads, reread_error) = if accepted {
            reread_threads(&client)
        } else {
            (None, Vec::new(), None)
        };
        let _ = sender.send(Fetched::ThreadOp {
            id,
            verb,
            accepted,
            error,
            rows,
            threads,
            reread_error,
        });
    });
    receiver
}

/// One space's unshare, on a worker thread: a single path-only DELETE, never
/// retried, then the Library listing read again. The DELETE's answer is only
/// acceptance; the rows in the reply are the proof of what changed.
fn fresh_unshare(client: Option<AccountClient>, slug: String) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(client) = client else {
            let _ = sender.send(Fetched::Failed("no account link in this app".into()));
            return;
        };
        let (accepted, error) = match client.space_unshare(&slug) {
            Ok(answer) => (answer.accepted, None),
            Err(failure) => (false, Some(failure.to_string())),
        };
        let (listing, reread_error) = match client.artifacts() {
            Ok(listing) => (Some((listing.artifacts, listing.total_count)), None),
            Err(failure) => (None, Some(failure.to_string())),
        };
        let _ = sender.send(Fetched::Unshare {
            slug,
            accepted,
            error,
            listing,
            reread_error,
        });
    });
    receiver
}

/// Export one space on a worker thread, for its preview window. A read, never
/// a write: there is no arm, and no listing to read again.
fn fresh_space_open(
    client: Option<AccountClient>,
    slug: String,
    name: String,
) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let exported = match client {
            Some(client) => client
                .space_export(&slug)
                .map_err(|failure| failure.to_string()),
            None => Err("no account link in this app".to_owned()),
        };
        let _ = sender.send(Fetched::SpaceExport { name, exported });
    });
    receiver
}

/// Delete one space on a worker thread, then read the listing again: the
/// DELETE's acceptance and the rows as they now are travel together.
fn fresh_space_delete(client: Option<AccountClient>, slug: String) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(client) = client else {
            let _ = sender.send(Fetched::Failed("no account link in this app".into()));
            return;
        };
        let (accepted, error) = match client.space_delete(&slug) {
            Ok(answer) => (answer.accepted, None),
            Err(failure) => (false, Some(failure.to_string())),
        };
        let (listing, reread_error) = match client.artifacts() {
            Ok(listing) => (Some((listing.artifacts, listing.total_count)), None),
            Err(failure) => (None, Some(failure.to_string())),
        };
        let _ = sender.send(Fetched::SpaceDelete {
            slug,
            accepted,
            error,
            listing,
            reread_error,
        });
    });
    receiver
}

/// Read the Ideas page from the service on a worker thread.
fn fresh_ideas(client: Option<AccountClient>) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let fetched = match client {
            None => Fetched::Failed("no account link in this app".into()),
            Some(client) => match client.ideas() {
                Ok(listing) => Fetched::Ideas(HeldIdeas {
                    ideas: listing.items,
                    total: listing.total,
                    loaded: true,
                    error: None,
                }),
                Err(error) => Fetched::Failed(error.to_string()),
            },
        };
        let _ = sender.send(fetched);
    });
    receiver
}

/// Run one account-wide conversation search on a worker thread: the same read
/// the native page makes, and the only page whose read carries the reader's
/// own words.
fn fresh_search(client: Option<AccountClient>, query: String) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let fetched = match client {
            None => Fetched::Failed("no account link in this app".into()),
            Some(client) => match client.search(&query) {
                Ok(listing) => Fetched::Search(crate::pages::held_search_from(&query, &listing)),
                Err(error) => Fetched::Failed(error.to_string()),
            },
        };
        let _ = sender.send(fetched);
    });
    receiver
}

/// Read one cached hero off the main loop. The cache lookup and the file
/// read both happen on the worker; the answer is a data URI or None.
fn read_hero(url: String) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(Fetched::Hero(feed_hero(&url)));
    });
    receiver
}

/// Read the Feed listing from the service on a worker thread. The rows keep
/// the native page's wording, and the graphic rule is the native one: the
/// first two graphical cards render live, the rest read as their text.
fn fresh_feed(client: Option<AccountClient>) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let fetched = match client {
            None => Fetched::Failed("no account link in this app".into()),
            Some(client) => match client.feed() {
                Ok(listing) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|span| span.as_secs() as i64)
                        .unwrap_or(0);
                    Fetched::Feed(crate::pages::held_feed_from(
                        &listing,
                        now,
                        crate::widget::widget_available(),
                    ))
                }
                Err(error) => Fetched::Failed(error.to_string()),
            },
        };
        let _ = sender.send(fetched);
    });
    receiver
}

/// Read the Threads listing from the service on a worker thread. The
/// account listing carries every conversation, so this is the same read the
/// native connect makes -- expensive, which is why Refresh leaves the loop.
fn fresh_threads(client: Option<AccountClient>) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let fetched = match client {
            None => Fetched::Failed("no account link in this app".into()),
            Some(client) => match client.connect() {
                Ok(snapshot) => Fetched::Threads(crate::pages::held_threads_from(
                    &snapshot.threads,
                    &crate::app::local_today(),
                )),
                Err(error) => Fetched::Failed(error.to_string()),
            },
        };
        let _ = sender.send(fetched);
    });
    receiver
}

/// Read Goals and its Tracking section on one worker thread, the way the
/// native fill reads both under one page ticket. A failed roster fails the
/// command; a failed Tracking read keeps the roster and travels as its own
/// sentence, so the page never reads an unread section as an empty one.
fn fresh_goals(client: Option<AccountClient>) -> Receiver<Fetched> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let fetched = match client {
            None => Fetched::Failed("no account link in this app".into()),
            Some(client) => match client.goals() {
                Err(error) => Fetched::Failed(error.to_string()),
                Ok(listing) => {
                    let held = match client.goals_tracking() {
                        Ok(tracked) => HeldGoals {
                            goals: listing.items,
                            goals_total: listing.total,
                            tracking: tracked.items,
                            tracking_total: tracked.total,
                            tracking_error: None,
                        },
                        Err(error) => HeldGoals {
                            goals: listing.items,
                            goals_total: listing.total,
                            tracking: Vec::new(),
                            tracking_total: 0,
                            tracking_error: Some(error.to_string()),
                        },
                    };
                    Fetched::Goals(held)
                }
            },
        };
        let _ = sender.send(fetched);
    });
    receiver
}

/// When this process started, for the probe's elapsed numbers. Set once from
/// `main`, so every window logs the same clock.
static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub(crate) fn mark_start() {
    let _ = PROCESS_START.set(Instant::now());
}

fn since_start_ms() -> u128 {
    PROCESS_START
        .get()
        .map(|start| start.elapsed().as_millis())
        .unwrap_or(0)
}

/// Whether a command earns a line in the launch log. The Chat page re-asks
/// for its cached transcript on a timer, so a mirror window left open would
/// otherwise fill the log with the same line thousands of times; each cache
/// read is named once per window instead. A trip to the service is named
/// every time -- it is the thing a launch log is actually for, and it is
/// rare. The cache/wire split is the command's own suffix, the same one the
/// page's contract uses: `.refresh`, `.run`, `.send` and `.stop` go out, the
/// rest are answered from held rows.
pub(crate) fn should_log_command(
    command: &str,
    named: &mut std::collections::HashSet<String>,
) -> bool {
    let wire = command.ends_with(".refresh")
        || command.ends_with(".run")
        || command.ends_with(".send")
        || command.ends_with(".stop")
        || command.ends_with(".decide")
        || command.ends_with(".delete")
        || command.ends_with(".set")
        || command.ends_with(".reset")
        || command.ends_with(".grant")
        || command.ends_with(".start")
        || command.ends_with(".open")
        || command.ends_with(".new");
    wire || named.insert(command.to_owned())
}

/// The page's staged quote as the wire carries it: the turn it answers and
/// the line the page showed. Not trusted: `web_send` runs it through the
/// composer's own guard before the app stages anything.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub(crate) struct WebQuote {
    pub id: String,
    pub excerpt: String,
}

pub(crate) type WebSend = std::rc::Rc<dyn Fn(&str, Option<WebQuote>) -> Result<(), String>>;
pub(crate) type WebStop = std::rc::Rc<dyn Fn() -> Result<(), String>>;
pub(crate) type WebAttach = std::rc::Rc<dyn Fn() -> Result<(), String>>;
pub(crate) type WebUnstage = std::rc::Rc<dyn Fn(&str) -> Result<(), String>>;
pub(crate) type WebApprovals = std::rc::Rc<dyn Fn() -> Result<(), String>>;
pub(crate) type WebDecide = std::rc::Rc<dyn Fn(&str, &str) -> Result<(), String>>;
/// The turn menu's Delete press: the app's own armed two-press delete, on the
/// latest turn it names.
pub(crate) type WebTurnDelete = std::rc::Rc<dyn Fn() -> Result<(), String>>;
/// The page's typing, written into the app's own composer so both surfaces show
/// one draft.
pub(crate) type WebDraft = std::rc::Rc<dyn Fn(&str) -> Result<(), String>>;
/// The mic pill: the app's own toggle, so a take started here and one started
/// in the native window are the same take.
pub(crate) type WebDictate = std::rc::Rc<dyn Fn() -> Result<(), String>>;
pub(crate) type WebHistory = std::rc::Rc<dyn Fn() -> Result<Vec<crate::pages::HistoryRow>, String>>;
pub(crate) type WebOpenThread = std::rc::Rc<dyn Fn(Option<&str>) -> Result<(), String>>;
pub(crate) type WebSetPolicy = std::rc::Rc<dyn Fn(&str, &str) -> Result<(), String>>;
pub(crate) type WebResetConnector = std::rc::Rc<dyn Fn(&str) -> Result<(), String>>;
/// Grant one folder: the path the page's own sheet returned, then the two
/// boundaries the native page passes.
pub(crate) type WebHostGrant = std::rc::Rc<dyn Fn(&str, bool, bool) -> Result<(), String>>;
pub(crate) type WebHostStart = std::rc::Rc<dyn Fn(bool) -> Result<(), String>>;
pub(crate) type WebHostStop = std::rc::Rc<dyn Fn() -> Result<(), String>>;
/// Allow once / Deny on one pending write: the card's own id, never a path.
pub(crate) type WebHostDecide = std::rc::Rc<dyn Fn(&str, bool) -> Result<(), String>>;
/// The saved write permission: writes in granted read-write folders without a
/// per-write card, saved durably first and enforced after.
pub(crate) type WebHostPolicy = std::rc::Rc<dyn Fn(bool) -> Result<(), String>>;
/// Clear the session log. Display only: cards and grants are untouched.
pub(crate) type WebHostClearLog = std::rc::Rc<dyn Fn() -> Result<(), String>>;
/// The page's filename search over the granted folders: the app validates the
/// query and queues the walk; the page reads the answer from its next snapshot.
pub(crate) type WebHostSearch = std::rc::Rc<dyn Fn(&str) -> Result<(), String>>;
/// One hit's text, for the page's preview: the broker's own bounded read.
pub(crate) type WebHostPreview =
    std::rc::Rc<dyn Fn(&str, &str) -> Result<serde_json::Value, String>>;
/// The pump a window needs when the Local host page is its only host surface:
/// poll the controller and re-project, so a queued grant, search or approval
/// lands without a native page being open to do it.
pub(crate) type WebHostRefresh = std::rc::Rc<dyn Fn() -> Result<(), String>>;
/// The page's content search over the granted folders: the app validates the
/// query and queues the read; the answer travels in the next snapshot.
pub(crate) type WebHostContent = std::rc::Rc<dyn Fn(&str) -> Result<(), String>>;

/// One press on the sign-in page, with the values the page typed in. The press
/// names which button; the app's sign-in module validates and saves, so the
/// page cannot hold the rules.
#[derive(Clone, Debug, Default)]
pub(crate) struct SigninPress {
    pub press: String,
    pub python: String,
    pub folder: String,
    pub consent: bool,
}

pub(crate) type WebSigninPress = std::rc::Rc<dyn Fn(SigninPress) -> Result<(), String>>;
/// Clear the saved link and hand the app back to its sign-in surface: the
/// native Settings button's own press, from a page.
pub(crate) type WebSignOut = std::rc::Rc<dyn Fn() -> Result<(), String>>;
/// Run the app's own update check.
pub(crate) type WebUpdateCheck = std::rc::Rc<dyn Fn() -> Result<(), String>>;
/// The update's one action: download what was offered, or restart into what is
/// already staged and verified.
pub(crate) type WebUpdateInstall = std::rc::Rc<dyn Fn() -> Result<(), String>>;

/// The routes a page has back into the app for its controls. None is a
/// dispatcher of its own: a send goes through the app's single sender, Stop
/// takes no session ticket at all (which is the point of stop, since it has
/// to fire while a send holds the `Send` slot), a paste is the app's own
/// clipboard read, and a decision is the app's one `decide` path. The page
/// gains no route to the session slot, the composer or the service.
pub(crate) struct WebRoutes {
    pub send: WebSend,
    pub stop: WebStop,
    /// Stage the clipboard's picture, the app's `+` press.
    pub attach: WebAttach,
    /// Drop one staged picture by name, the chip's ×.
    pub unstage: WebUnstage,
    /// Re-read the pending approvals, the panel's own read.
    pub approvals: WebApprovals,
    /// Decide one pending approval: the service's own outcome word.
    pub decide: WebDecide,
    /// Delete the latest turn: the app's own arm-then-confirm press, so a
    /// press here and a press in the turn menu advance one rule.
    pub turn_delete: WebTurnDelete,
    /// Write the page's typing into the app's composer draft.
    pub draft: WebDraft,
    /// Start or stop a dictation take: the app's own mic toggle.
    pub dictate: WebDictate,
    /// The drawer's History rows: the panel's own listing, in its own words.
    pub history: WebHistory,
    /// Open a thread from History, or a fresh one with no id: the native
    /// activation's call.
    pub open_thread: WebOpenThread,
    /// Flip one policy row: the same table and the same update call as the
    /// chat panel's own press.
    pub set_policy: WebSetPolicy,
    /// Reset one connector's grants: the chat panel's own armed press, minus
    /// the arming, which the page holds.
    pub reset_connector: WebResetConnector,
    /// Grant a folder the page chose on its own sheet; the page gets the
    /// outcome from its next snapshot (or from the refusal this answers).
    pub host_grant: WebHostGrant,
    /// Start the local host, carrying the page's consent.
    pub host_start: WebHostStart,
    /// Stop the host and revoke its grants.
    pub host_stop: WebHostStop,
    pub host_decide: WebHostDecide,
    pub host_policy: WebHostPolicy,
    pub host_clear_log: WebHostClearLog,
    /// The local host page's filename search and one hit's preview: the app's
    /// own reads over the folders the person granted.
    pub host_search: WebHostSearch,
    pub host_preview: WebHostPreview,
    pub host_refresh: WebHostRefresh,
    pub host_content: WebHostContent,
    pub update_check: WebUpdateCheck,
    pub update_install: WebUpdateInstall,
    pub signin_press: WebSigninPress,
    /// Sign out: clear the saved link and return the app to sign-in.
    pub sign_out: WebSignOut,
}

/// The answer a mutating command gets: `ok` when the app took the press, and
/// the route's own sentence when it refused. A window opened without routes
/// cannot mutate at all, and says so in the page instead of doing nothing.
fn route_answer(result: Option<Result<(), String>>) -> Answer {
    match result {
        None => Answer::Now(serde_json::json!({
            "ok": false,
            "error": "this window cannot make that change"
        })),
        Some(Ok(())) => Answer::Now(serde_json::json!({ "ok": true })),
        Some(Err(sentence)) => Answer::Now(serde_json::json!({ "ok": false, "error": sentence })),
    }
}

/// What a folder sheet shows where it stands: the folder's own path, the one
/// above it, and its subfolders. Folders only, by design -- the app needs a
/// folder, never a file browser -- and one level at a time, so the page walks
/// the tree instead of being handed the disk. An empty path starts at the app's
/// own session folder, which is the default the sign-in form opens on.
pub(crate) fn folder_listing(raw: &str) -> serde_json::Value {
    let asked = raw.trim();
    if asked.is_empty() {
        return start_listing();
    }
    let path = std::path::PathBuf::from(asked);
    if !path.is_absolute() {
        return serde_json::json!({
            "ok": false,
            "error": "The folder path must be absolute."
        });
    }
    list_folder(&path)
}

/// The sheet's opening answer: the app's own session folder, or the home
/// folder when this clone has no config home to name.
fn start_listing() -> serde_json::Value {
    if let Some(dir) = crate::session::account_dir() {
        return list_folder(&dir);
    }
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) => list_folder(&home),
        None => serde_json::json!({
            "ok": false,
            "error": "No home folder to start from."
        }),
    }
}

/// The rows one level holds. Symlinks to folders count as folders (a session
/// folder may live behind one); a file never does, and neither does an entry
/// whose name is not readable as text.
fn list_folder(path: &std::path::Path) -> serde_json::Value {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            return serde_json::json!({
                "ok": false,
                "error": format!("Cannot read that folder: {error}")
            });
        }
    };
    let mut folders: Vec<(String, String)> = Vec::new();
    let mut capped = false;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let is_folder = kind.is_dir() || (kind.is_symlink() && entry.path().is_dir());
        if !is_folder {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        folders.push((name, entry.path().display().to_string()));
        if folders.len() >= MAX_FOLDER_ROWS {
            capped = true;
            break;
        }
    }
    folders.sort_by(|left, right| {
        left.0
            .to_lowercase()
            .cmp(&right.0.to_lowercase())
            .then_with(|| left.0.cmp(&right.0))
    });
    let folders: Vec<serde_json::Value> = folders
        .into_iter()
        .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
        .collect();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.display().to_string());
    serde_json::json!({
        "ok": true,
        "path": path.display().to_string(),
        "parent": parent,
        "folders": folders,
        "note": capped.then_some("Showing the first 500 folders."),
    })
}

/// Make one folder inside the folder the sheet stands in, and answer with the
/// folder made so the sheet can step into it.
///
/// The native chooser this sheet replaced had a Create Folder button, and so
/// does this one: without it a person who has no folder yet cannot get one from
/// inside the app. What it refuses it says in the app's own words, and it
/// refuses before touching the disk: an empty name, a name with a separator or
/// a control character, `.`/`..`, a parent that is not absolute or not a folder.
/// The parent may be missing: a first launch's own session folder does not
/// exist until something writes in it, and the sheet opens on exactly that
/// path. Creating the chain is what the writer does anyway when it saves a
/// link, so a folder made here is the app's own folder made early -- never a
/// path the person did not type into this sheet.
pub(crate) fn folder_create(raw_parent: &str, raw_name: &str) -> serde_json::Value {
    let asked = raw_parent.trim();
    if !asked.is_empty() && !std::path::Path::new(asked).is_absolute() {
        return serde_json::json!({
            "ok": false,
            "error": "The folder path must be absolute."
        });
    }
    let parent = if asked.is_empty() {
        match crate::session::account_dir() {
            Some(dir) => dir,
            None => {
                return serde_json::json!({
                    "ok": false,
                    "error": "No home folder to create in."
                });
            }
        }
    } else {
        std::path::PathBuf::from(asked)
    };
    let name = raw_name.trim();
    if name.is_empty() {
        return serde_json::json!({
            "ok": false,
            "error": "Type a name for the new folder."
        });
    }
    if name == "." || name == ".." {
        return serde_json::json!({
            "ok": false,
            "error": "That is not a folder name."
        });
    }
    if name.contains('/') || name.contains('\\') {
        return serde_json::json!({
            "ok": false,
            "error": "A folder name cannot contain a slash."
        });
    }
    if name.chars().any(char::is_control) {
        return serde_json::json!({
            "ok": false,
            "error": "A folder name cannot contain control characters."
        });
    }
    if parent.exists() && !parent.is_dir() {
        return serde_json::json!({
            "ok": false,
            "error": "That path is a file, not a folder."
        });
    }
    let made = parent.join(name);
    // `create_dir_all` treats an existing folder as success, and that is not
    // what the press said it was doing: one new folder, and a name already
    // taken is the app's own sentence, whichever kind of entry holds it.
    if made.exists() {
        return serde_json::json!({
            "ok": false,
            "error": format!("{name} is already in this folder.")
        });
    }
    match std::fs::create_dir_all(&made) {
        Ok(()) => {
            // A session folder is private by the app's own rule -- the bridge
            // refuses a config folder other people can read (`insecure_config`)
            // -- so the folder this press makes is the writer's own 0700, not
            // whatever the umask would have left.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(&made, std::fs::Permissions::from_mode(0o700));
            }
            serde_json::json!({
                "ok": true,
                "path": made.display().to_string()
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            serde_json::json!({
                "ok": false,
                "error": format!("A folder named {name} is already in this folder.")
            })
        }
        Err(error) => serde_json::json!({
            "ok": false,
            "error": format!("Cannot create that folder: {error}")
        }),
    }
}

/// One page's folder list is a sheet, not a file manager: past this many rows
/// the answer says so rather than painting thousands.
const MAX_FOLDER_ROWS: usize = 500;

/// One command from the page. Reads only, and by name: an unknown command is
/// refused instead of guessed at, so a typo in the UI is visible in the page
/// and in the log. `routes` is the Chat window's way back into the app; every
/// other page passes `None` and cannot mutate at all.
pub(crate) fn handle_command_with(
    raw: &str,
    client: Option<AccountClient>,
    routes: Option<&WebRoutes>,
) -> (serde_json::Value, Answer, String) {
    let Ok(request) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (
            serde_json::json!(0),
            Answer::Now(serde_json::json!({ "ok": false, "error": "malformed request" })),
            "malformed".into(),
        );
    };
    let id = request.get("id").cloned().unwrap_or(serde_json::json!(0));
    let command = request
        .get("cmd")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_owned();
    let answer = match command.as_str() {
        "library.snapshot" => {
            let (artifacts, total) = crate::pages::library_snapshot();
            Answer::Now(serde_json::json!({
                "ok": true,
                "artifacts": artifacts,
                "total": total,
                "armed": crate::pages::unshare_armed(),
                "delete_armed": crate::pages::space_delete_armed(),
                "loaded": crate::pages::library_loaded(),
            }))
        }
        "library.refresh" => Answer::Later(fresh_listing(client)),
        // A space's unshare, from its own row: one armed press, then one
        // path-only DELETE, then the listing read again. The row is addressed
        // by the handle the held listing carries -- never a ref the page made
        // up -- and the first press writes nothing at all.
        "library.unshare" => {
            let slug = request
                .get("slug")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let held = crate::pages::library_snapshot().0;
            let known = held.iter().any(|row| {
                row.target_kind.as_deref() == Some("space") && row.slug.as_deref() == Some(slug)
            });
            if !known {
                Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "that space is not in the held Library listing",
                }))
            } else if !crate::pages::unshare_press(slug) {
                let (artifacts, total) = crate::pages::library_snapshot();
                Answer::Now(serde_json::json!({
                    "ok": true,
                    "accepted": false,
                    "slug": slug,
                    "armed": slug,
                    "loaded": crate::pages::library_loaded(),
                    "artifacts": artifacts,
                    "total": total,
                }))
            } else {
                Answer::Later(fresh_unshare(client, slug.to_owned()))
            }
        }
        // Deleting a space is its own route with its own arm, so neither
        // confirm can spend the other. It is not named `library.delete`:
        // that name still means deleting an artifact, which stays unbuilt
        // and unknown.
        "library.delete-space" => {
            let slug = request
                .get("slug")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let held = crate::pages::library_snapshot().0;
            let known = held.iter().any(|row| {
                row.target_kind.as_deref() == Some("space") && row.slug.as_deref() == Some(slug)
            });
            if !known {
                Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "that space is not in the held Library listing",
                }))
            } else if !crate::pages::space_delete_press(slug) {
                let (artifacts, total) = crate::pages::library_snapshot();
                Answer::Now(serde_json::json!({
                    "ok": true,
                    "accepted": false,
                    "slug": slug,
                    "delete_armed": slug,
                    "armed": crate::pages::unshare_armed(),
                    "loaded": crate::pages::library_loaded(),
                    "artifacts": artifacts,
                    "total": total,
                }))
            } else {
                Answer::Later(fresh_space_delete(client, slug.to_owned()))
            }
        }
        // A space's own Open: the page asks for the export the native row's
        // Export button fetches, and the app draws the preview window, so the
        // service's markup is never rendered by the page itself. A read, so
        // there is no arm and nothing is written.
        "library.open-space" => {
            let slug = request
                .get("slug")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let held = crate::pages::library_snapshot().0;
            let known = held
                .iter()
                .find(|row| {
                    row.target_kind.as_deref() == Some("space") && row.slug.as_deref() == Some(slug)
                })
                .map(|row| row.title.clone());
            match known {
                Some(title) => Answer::Later(fresh_space_open(client, slug.to_owned(), title)),
                None => Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "that space is not in the held Library listing",
                })),
            }
        }
        "goals.snapshot" => {
            let held = crate::pages::goals_snapshot();
            Answer::Now(serde_json::json!({
                "ok": true,
                "goals": held.goals,
                "goals_total": held.goals_total,
                "tracking": held.tracking,
                "tracking_total": held.tracking_total,
                "tracking_error": held.tracking_error,
                "loaded": crate::pages::goals_loaded(),
            }))
        }
        "goals.refresh" => Answer::Later(fresh_goals(client)),
        "tasks.snapshot" => {
            // Local reminders cost no wire trip, and a page open should see
            // the store as it is now rather than as of the last fill.
            crate::pages::tasks_local_refresh();
            Answer::Now(tasks_reply(&crate::pages::tasks_snapshot()))
        }
        "tasks.refresh" => Answer::Later(fresh_tasks(client)),
        // Deleting one schedule, addressed by the held row's own job id: the
        // same two-press rule the conversation rows carry. The first press
        // arms and writes nothing; the second sends the one `tasks.delete` and
        // the Tasks listing is read again, which is the only proof it is gone.
        "tasks.delete" => {
            let job = request
                .get("job")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let held = crate::pages::tasks_snapshot();
            let known = held.schedules.iter().any(|row| row.id == job);
            if !known {
                Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "that schedule is not in the held Tasks listing",
                }))
            } else if !crate::pages::task_delete_press(job) {
                let mut reply = tasks_payload(&held);
                reply["ok"] = serde_json::json!(true);
                reply["accepted"] = serde_json::json!(false);
                reply["armed"] = serde_json::json!(job);
                reply["job"] = serde_json::json!(job);
                Answer::Now(reply)
            } else {
                Answer::Later(fresh_task_delete(client, job.to_owned()))
            }
        }
        // Upcoming, from the state the panel holds: the next run the last
        // Tasks read named and the approvals the panel last painted. No wire
        // read of its own, so a page open costs nothing and cannot queue
        // behind a listing; `schedules_loaded` separates "nothing scheduled"
        // from "nobody has read it yet".
        "upcoming.snapshot" => {
            let tasks = crate::pages::tasks_snapshot();
            Answer::Now(serde_json::json!({
                "ok": true,
                "next_run": tasks.next_run,
                "schedules_loaded": tasks.loaded,
                "approvals": crate::pages::approvals_view(),
            }))
        }
        "ideas.snapshot" => Answer::Now(ideas_reply(&crate::pages::ideas_snapshot())),
        "ideas.refresh" => Answer::Later(fresh_ideas(client)),
        "threads.snapshot" => Answer::Now(threads_reply(&crate::pages::threads_snapshot())),
        "threads.refresh" => Answer::Later(fresh_threads(client)),
        // One conversation's change, from its own row. The id must be one the
        // held listing carries -- never one the page made up -- and a rename
        // must carry a real title. The open thread's own id is the listing's,
        // so a stale row is refused rather than sent on the wire.
        // One row's unread badge, cleared: the same shape as the conversation
        // changes -- the id must be one the held listing carries, the write
        // runs off the main loop, and the listing read back is the proof.
        "threads.seen" => {
            let id = request
                .get("thread")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let held = crate::pages::threads_snapshot();
            if !held.threads.iter().any(|row| row.id == id) {
                Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "that conversation is not in the held listing",
                }))
            } else {
                Answer::Later(fresh_thread_seen(client, id.to_owned()))
            }
        }
        "threads.op" => {
            // The row is named by `thread`, not `id`: `id` is the channel's own
            // reply ticket and the page refuses to overwrite it.
            let id = request
                .get("thread")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let verb = request
                .get("verb")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let title = request
                .get("title")
                .and_then(|value| value.as_str())
                .map(str::trim);
            let held = crate::pages::threads_snapshot();
            let known = held.threads.iter().any(|row| row.id == id);
            let planned: Result<Option<String>, String> = if !known {
                Err("that conversation is not in the held listing".to_owned())
            } else {
                match verb {
                    "pin" | "unpin" | "archive" | "unarchive" | "delete" => Ok(None),
                    "rename" => match title {
                        Some("") => Err("a thread needs a non-blank title".to_owned()),
                        Some(name) if name.chars().count() > MAX_THREAD_TITLE => Err(format!(
                            "a thread title is at most {MAX_THREAD_TITLE} characters"
                        )),
                        Some(name) => Ok(Some(name.to_owned())),
                        None => Err("a rename needs a title".to_owned()),
                    },
                    _ => Err("that is not a conversation change this app makes".to_owned()),
                }
            };
            match planned {
                Err(sentence) => Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": sentence,
                })),
                // A delete is spent by two presses on the same row: the first
                // arms it and sends nothing at all, the second confirms. Every
                // other verb is one press and clears the arm on its way.
                Ok(title) if verb == "delete" => {
                    if !crate::pages::thread_delete_press(id) {
                        let held = crate::pages::threads_snapshot();
                        Answer::Now(serde_json::json!({
                            "ok": true,
                            "accepted": false,
                            "armed": id,
                            "thread": id,
                            "verb": verb,
                            "threads": held.threads,
                            "total": held.total,
                            "loaded": held.loaded,
                        }))
                    } else {
                        Answer::Later(fresh_thread_op(
                            client,
                            id.to_owned(),
                            verb.to_owned(),
                            title,
                        ))
                    }
                }
                Ok(title) => {
                    crate::pages::thread_delete_forget();
                    Answer::Later(fresh_thread_op(
                        client,
                        id.to_owned(),
                        verb.to_owned(),
                        title,
                    ))
                }
            }
        }
        "feed.snapshot" => Answer::Now(feed_reply(&crate::pages::feed_snapshot())),
        "feed.refresh" => Answer::Later(fresh_feed(client)),
        // Settings answers from held state only: the native page owns the
        // presses that write, and the policy is whatever the chat panel last
        // read rather than a second trip here.
        "settings.snapshot" => Answer::Now(settings_reply(&crate::pages::settings_snapshot())),
        // The app's one sign-out press, whichever surface makes it.
        "settings.sign_out" => route_answer(routes.map(|routes| (routes.sign_out)())),
        // The host polls its own controller every 250 ms and the native paint
        // re-projects each time, so this answer follows that pump: the page
        // re-asks and sees new cards and log lines without holding a lock.
        "localhost.snapshot" => {
            // The pump, for a window whose only host surface is this page: the
            // native page polls its controller on its own timer, and without
            // one open a queued grant or search would never land. The answer is
            // still the projection, re-taken after the poll.
            if let Some(routes) = routes {
                let _ = (routes.host_refresh)();
            }
            Answer::Now(localhost_reply(&crate::pages::localhost_snapshot()))
        }
        "chat.snapshot" => Answer::Now(chat_reply(&crate::pages::chat_snapshot())),
        "shell.snapshot" => Answer::Now(shell_reply(routes)),
        // Send dispatches nothing itself: the draft goes back to the
        // controller that owns the `Send` slot, which is the path the app's
        // own Send button takes -- same slot, same receipt, same history
        // reload. The answer says whether the app took the press; what the
        // service said arrives in the transcript and the status line the page
        // already follows.
        "chat.send" => {
            let text = request
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            // The staged quote, if the page's Reply put one up. It is passed
            // as data and guarded by the app, never trusted as a row: the page
            // says which turn it is answering, the composer decides whether
            // that is a quotable turn.
            let quote = request
                .get("quote")
                .and_then(|value| serde_json::from_value(value.clone()).ok());
            match routes {
                None => Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "this window cannot send"
                })),
                Some(routes) => match (routes.send)(text, quote) {
                    Ok(()) => Answer::Now(serde_json::json!({ "ok": true })),
                    Err(sentence) => {
                        Answer::Now(serde_json::json!({ "ok": false, "error": sentence }))
                    }
                },
            }
        }
        // Stop, the other control the native card has. It goes back to the app
        // the same way, and the app's stop takes no ticket -- so pressing this
        // while a send holds the `Send` slot still reaches the service. The
        // answer says only that the cancel went out; whether anything was
        // running is the service's word, and the transcript and the status
        // line carry what actually landed.
        "chat.stop" => match routes {
            None => Answer::Now(serde_json::json!({
                "ok": false,
                "error": "this window cannot stop"
            })),
            Some(routes) => match (routes.stop)() {
                Ok(()) => Answer::Now(serde_json::json!({ "ok": true })),
                Err(sentence) => Answer::Now(serde_json::json!({ "ok": false, "error": sentence })),
            },
        },
        // A picture for the next send. The page cannot read the clipboard and
        // must not carry bytes: it asks for the app's own paste, and the chip
        // it then shows comes back from `chat.snapshot` with the send.
        "chat.attach" => match routes {
            None => Answer::Now(serde_json::json!({
                "ok": false,
                "error": "this window cannot attach a picture"
            })),
            Some(routes) => match (routes.attach)() {
                Ok(()) => Answer::Now(serde_json::json!({ "ok": true })),
                Err(sentence) => Answer::Now(serde_json::json!({ "ok": false, "error": sentence })),
            },
        },
        // The chip's ×: one staged picture dropped by name, before any send.
        "chat.unstage" => {
            let name = request
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            match routes {
                None => Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "this window cannot drop a picture"
                })),
                Some(routes) => match (routes.unstage)(name) {
                    Ok(()) => Answer::Now(serde_json::json!({ "ok": true })),
                    Err(sentence) => {
                        Answer::Now(serde_json::json!({ "ok": false, "error": sentence }))
                    }
                },
            }
        }
        // The pending list, read fresh: the panel's own "Check approvals"
        // dispatch. The rows arrive in the next `chat.snapshot`, so the page
        // never holds a second copy of the service's answer.
        "chat.approvals.refresh" => match routes {
            None => Answer::Now(serde_json::json!({
                "ok": false,
                "error": "this window cannot read approvals"
            })),
            Some(routes) => match (routes.approvals)() {
                Ok(()) => Answer::Now(serde_json::json!({ "ok": true })),
                Err(sentence) => Answer::Now(serde_json::json!({ "ok": false, "error": sentence })),
            },
        },
        // One decision, by the service's own outcome word. The app's `decide`
        // path re-reads the list afterwards; the page's cards follow that
        // read, so a request leaves the page only once the service says so.
        "chat.decide" => {
            // `approval_id`, never `id`: the envelope's own `id` is the reply
            // ticket, and a request that overwrote it would never be answered
            // on the page.
            let approval_id = request
                .get("approval_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let word = request
                .get("decision")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            match routes {
                None => Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "this window cannot decide approvals"
                })),
                Some(routes) => match (routes.decide)(approval_id, word) {
                    Ok(()) => Answer::Now(serde_json::json!({ "ok": true })),
                    Err(sentence) => {
                        Answer::Now(serde_json::json!({ "ok": false, "error": sentence }))
                    }
                },
            }
        }
        // The turn menu's delete: the app's own two-press delete, so the page's
        // press arms and confirms the same turn the native button does. The
        // page reads its own label from the snapshot, so this press never has
        // to guess which of the two presses it is.
        "chat.delete" => route_answer(routes.map(|routes| (routes.turn_delete)())),
        // The composer's two controls the page shares with the window: the
        // draft it types into is the app's own, and the mic is the app's own
        // toggle -- one draft, one take, whichever surface pressed.
        "chat.draft" => {
            let text = request
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            route_answer(routes.map(|routes| (routes.draft)(text)))
        }
        "chat.dictate" => route_answer(routes.map(|routes| (routes.dictate)())),
        // The drawer's History: the panel's own listing, and the same open the
        // native activation makes. A window without routes cannot read it and
        // says so, rather than claiming an empty account.
        "history.snapshot" => match routes {
            None => Answer::Now(serde_json::json!({
                "ok": false,
                "error": "this window cannot read history"
            })),
            Some(routes) => match (routes.history)() {
                Ok(rows) => Answer::Now(serde_json::json!({ "ok": true, "rows": rows })),
                Err(sentence) => Answer::Now(serde_json::json!({ "ok": false, "error": sentence })),
            },
        },
        "history.open" => {
            let thread = request.get("thread").and_then(|value| value.as_str());
            route_answer(routes.map(|routes| (routes.open_thread)(thread)))
        }
        // A fresh thread is History's own open with no id.
        "chat.new" => route_answer(routes.map(|routes| (routes.open_thread)(None))),
        // The Settings page's flips and resets: the same table, the same
        // update call and the same ticket the chat panel's presses take, so a
        // page press and a panel press cannot disagree or run two at once.
        "settings.policy.set" => {
            let label = request
                .get("label")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let value = request
                .get("value")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            route_answer(routes.map(|routes| (routes.set_policy)(label, value)))
        }
        "settings.connector.reset" => {
            let connector = request
                .get("connector")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            route_answer(routes.map(|routes| (routes.reset_connector)(connector)))
        }
        // The Local host page's three writes. Granting still opens the app's
        // own folder chooser -- the page cannot browse the disk, and the
        // choice is the person's -- but the grant itself is the same call the
        // native page makes, and its outcome arrives in the next snapshot.
        "localhost.grant" => {
            let folder = request
                .get("folder")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let consent = request
                .get("consent")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let writable = request
                .get("writable")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            route_answer(routes.map(|routes| (routes.host_grant)(folder, consent, writable)))
        }
        "localhost.start" => {
            let consent = request
                .get("consent")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            route_answer(routes.map(|routes| (routes.host_start)(consent)))
        }
        "localhost.stop" => route_answer(routes.map(|routes| (routes.host_stop)())),
        // The card's own decision, the saved permission and the log: the same
        // controller methods the native page's presses call, so a card approved
        // in the page and one approved in the app are one decision, and an
        // expired card is refused here by the same check as there.
        "localhost.decide" => {
            let id = request
                .get("approval_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let allow = request
                .get("allow")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            route_answer(routes.map(|routes| (routes.host_decide)(id, allow)))
        }
        "localhost.policy" => {
            let allow = request
                .get("allow")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            route_answer(routes.map(|routes| (routes.host_policy)(allow)))
        }
        "localhost.clear_log" => route_answer(routes.map(|routes| (routes.host_clear_log)())),
        // The page's own filename search: a read over the folders the person
        // granted. The app validates the query (an empty one, a path-shaped
        // one, an overlong one is refused by name), queues the walk and
        // reprojects, so the page's next snapshot carries the answer.
        "localhost.search" => {
            let query = request
                .get("query")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            route_answer(routes.map(|routes| (routes.host_search)(query)))
        }
        // The page's content search: the broker's own matching lines, read
        // over the folders the person granted and reported in the next
        // snapshot, exactly as the native card reads it.
        "localhost.content" => {
            let query = request
                .get("query")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            route_answer(routes.map(|routes| (routes.host_content)(query)))
        }
        // One hit's text: the same bounded read the native row makes, with the
        // path it came from. A refusal is the broker's sentence, never empty
        // content pretending to be a file.
        "localhost.preview" => {
            let folder = request
                .get("folder")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let path = request
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            match routes.map(|routes| (routes.host_preview)(folder, path)) {
                None => Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "this window cannot read your folders"
                })),
                Some(Ok(mut preview)) => {
                    preview["ok"] = serde_json::json!(true);
                    Answer::Now(preview)
                }
                Some(Err(sentence)) => {
                    Answer::Now(serde_json::json!({"ok": false, "error": sentence}))
                }
            }
        }
        // The update surface: the two presses the native card offers. The check
        // and the download run in the app, on its own threads; the page reads
        // their outcome from `settings.snapshot`, like the rest of the card.
        "update.check" => route_answer(routes.map(|routes| (routes.update_check)())),
        "update.install" => route_answer(routes.map(|routes| (routes.update_install)())),
        // One folder's subfolders, for the folder sheet a page stands in. A
        // read by path, with no ticket of its own: what it answers is the list
        // the sheet paints, and nothing is written.
        "folders.list" => {
            let path = request
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            Answer::Now(folder_listing(path))
        }
        // The sheet's create press: one new folder inside the folder the sheet
        // stands in. Not recursive -- the app only makes a child of a listing
        // it just read -- and only a child: the name is one component, so a
        // typed `..` or a slash cannot walk the write anywhere else.
        "folders.create" => {
            let path = request
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let name = request
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            Answer::Now(folder_create(path, name))
        }
        // The sign-in surface: the page owns the typing, the app owns the
        // rules. One press per message, carrying the values that press used.
        "signin.snapshot" => Answer::Now(signin_reply()),
        "signin.press" => {
            let press = request
                .get("press")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_owned();
            let python = request
                .get("python")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_owned();
            let folder = request
                .get("folder")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_owned();
            let consent = request
                .get("consent")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            route_answer(routes.map(|routes| {
                (routes.signin_press)(SigninPress {
                    press,
                    python,
                    folder,
                    consent,
                })
            }))
        }
        // The first-run step has nothing to press: it is a progress line, and
        // the page reads it like every other held state.
        "firstrun.snapshot" => Answer::Now(first_run_reply()),
        "search.snapshot" => Answer::Now(search_reply(&crate::pages::search_snapshot())),
        "search.run" => {
            let query = request
                .get("query")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            if query.is_empty() {
                // The native page refuses an empty query without a ticket, and
                // so does this one: no service read for no words.
                Answer::Now(serde_json::json!({
                    "ok": false,
                    "error": "type words to search for first"
                }))
            } else {
                Answer::Later(fresh_search(client, query))
            }
        }
        "feed.hero" => {
            let url = request
                .get("url")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            Answer::Later(read_hero(url.to_owned()))
        }
        other => Answer::Now(
            serde_json::json!({ "ok": false, "error": format!("unknown command: {other}") }),
        ),
    };
    (id, answer, command)
}

/// The reply a space write shares with its sibling: the service's acceptance,
/// and the rows as whatever listing read came after it said. A failed re-read
/// leaves the held rows in place and says the change is not proven rather
/// than painting them as the read.
fn space_write_reply(
    command: &str,
    slug: String,
    accepted: bool,
    error: Option<String>,
    listing: Option<(Vec<ArtifactSummary>, u64)>,
    reread_error: Option<String>,
    started: Instant,
) -> serde_json::Value {
    let reread = listing.is_some();
    let (artifacts, total) = match listing {
        Some((artifacts, total)) => {
            crate::pages::library_cache(artifacts.clone(), total);
            (artifacts, total)
        }
        None => crate::pages::library_snapshot(),
    };
    eprintln!(
        "muse webui: {command} space={slug} accepted={accepted} reread={reread} ms={}",
        started.elapsed().as_millis()
    );
    serde_json::json!({
        "ok": true,
        "accepted": accepted,
        "slug": slug,
        "armed": null,
        "delete_armed": null,
        // The rows may be the held ones when the re-read failed, so this says
        // what the cache actually holds, never what the press hoped.
        "loaded": crate::pages::library_loaded(),
        "error": error,
        "reread_error": reread_error,
        "artifacts": artifacts,
        "total": total,
    })
}

/// The reply a worker's answer becomes, and the cache it leaves behind: the
/// native page, the boot payload and this reply tell the same story
/// afterwards. Pure over the answer, so a test can read it without a window.
pub(crate) fn reply_for(fetched: Fetched, command: &str, started: Instant) -> serde_json::Value {
    match fetched {
        Fetched::Rows(artifacts, total) => {
            // A listing read disarms whatever row a confirm belonged to.
            crate::pages::unshare_forget();
            crate::pages::space_delete_forget();
            crate::pages::library_cache(artifacts.clone(), total);
            eprintln!(
                "muse webui: {command} read the service: rows={} total={} ms={}",
                artifacts.len(),
                total,
                started.elapsed().as_millis()
            );
            serde_json::json!({
                "ok": true, "artifacts": artifacts, "total": total,
                "armed": null, "delete_armed": null, "loaded": true
            })
        }
        Fetched::Unshare {
            slug,
            accepted,
            error,
            listing,
            reread_error,
        } => {
            // A confirm spends the arm whatever its answer was.
            crate::pages::unshare_forget();
            space_write_reply(
                command,
                slug,
                accepted,
                error,
                listing,
                reread_error,
                started,
            )
        }
        Fetched::SpaceDelete {
            slug,
            accepted,
            error,
            listing,
            reread_error,
        } => {
            crate::pages::space_delete_forget();
            space_write_reply(
                command,
                slug,
                accepted,
                error,
                listing,
                reread_error,
                started,
            )
        }
        Fetched::TaskDelete {
            job,
            accepted,
            error,
            held,
            reread_error,
        } => {
            // A confirm spends the arm whatever its answer was.
            crate::pages::task_delete_forget();
            let mut reply = match held {
                Some(held) => {
                    // The re-read is the listing the page holds from now on.
                    crate::pages::tasks_cache(held.clone());
                    let mut reply = tasks_reply(&held);
                    reply["loaded"] = serde_json::json!(true);
                    reply
                }
                None => {
                    let mut reply = tasks_reply(&crate::pages::tasks_snapshot());
                    reply["loaded"] = serde_json::json!(false);
                    reply
                }
            };
            reply["accepted"] = serde_json::json!(accepted);
            reply["job"] = serde_json::json!(job);
            reply["armed"] = serde_json::Value::Null;
            if let Some(error) = error {
                reply["error"] = serde_json::json!(error);
            }
            if let Some(reread_error) = reread_error {
                reply["reread_error"] = serde_json::json!(reread_error);
            }
            eprintln!(
                "muse webui: {command} delete accepted={accepted} job={job} after {}ms",
                started.elapsed().as_millis()
            );
            reply
        }
        Fetched::SpaceExport { name, exported } => match exported {
            Ok(exported) => {
                // The preview is the app's own window, drawn on the same main
                // loop this reply already runs on.
                crate::pages::space_preview_window(&name, &exported, None);
                eprintln!(
                    "muse webui: {command} opened {name} ({} bytes, truncated={}) after {}ms",
                    exported.size,
                    exported.truncated,
                    started.elapsed().as_millis()
                );
                serde_json::json!({
                    "ok": true,
                    "opened": true,
                    "title": name,
                    "format": exported.format,
                    "size": exported.size,
                    "truncated": exported.truncated,
                })
            }
            Err(error) => serde_json::json!({ "ok": false, "error": error }),
        },
        Fetched::Goals(mut held) => {
            // A failed Tracking read must not empty a section that is merely
            // stale: the rows the native page still shows stay in the cache
            // and in the reply, with the failure beside them.
            if held.tracking_error.is_some() && held.tracking.is_empty() {
                let previous = crate::pages::goals_snapshot();
                held.tracking = previous.tracking;
                held.tracking_total = previous.tracking_total;
            }
            crate::pages::goals_cache(held.clone());
            eprintln!(
                "muse webui: {command} read the service: goals={} goals_total={} \
                 tracking={} tracking_total={} ms={}",
                held.goals.len(),
                held.goals_total,
                held.tracking.len(),
                held.tracking_total,
                started.elapsed().as_millis()
            );
            serde_json::json!({
                "ok": true,
                "goals": held.goals,
                "goals_total": held.goals_total,
                "tracking": held.tracking,
                "tracking_total": held.tracking_total,
                "tracking_error": held.tracking_error,
                // The read that just cached these rows has landed, so a page
                // painting this reply can say "no goals yet" instead of
                // reusing the sentence for a roster nobody has read.
                "loaded": crate::pages::goals_loaded(),
            })
        }
        Fetched::Ideas(held) => {
            crate::pages::ideas_cache(held.clone());
            eprintln!(
                "muse webui: {command} read the service: ideas={} total={} ms={}",
                held.ideas.len(),
                held.total,
                started.elapsed().as_millis()
            );
            ideas_reply(&held)
        }
        Fetched::Tasks(held) => {
            // The listing painted again: a schedule's armed delete press does
            // not survive it, so a confirm aimed at a row the service has
            // since moved cannot delete what took its place.
            crate::pages::task_delete_forget();
            crate::pages::tasks_cache(held);
            upcoming_changed();
            // Local reminders are local state: the worker did not read them,
            // so they are re-read here, on the main loop.
            crate::pages::tasks_local_refresh();
            let held = crate::pages::tasks_snapshot();
            eprintln!(
                "muse webui: {command} read the service: schedules={} runs={} local={} ms={}",
                held.schedules.len(),
                held.runs.len(),
                held.local.len(),
                started.elapsed().as_millis()
            );
            tasks_reply(&held)
        }
        Fetched::Threads(held) => {
            crate::pages::threads_cache(held.clone());
            eprintln!(
                "muse webui: {command} read the service: threads={} total={} ms={}",
                held.threads.len(),
                held.total,
                started.elapsed().as_millis()
            );
            threads_reply(&held)
        }
        Fetched::ThreadOp {
            id,
            verb,
            accepted,
            error,
            rows,
            threads,
            reread_error,
        } => {
            if let Some(rows) = rows {
                crate::pages::threads_cache(rows);
                // The drawer is painted from the panel's own listing, so the
                // read-back lands there too rather than on the page alone.
                if let Some(chat) = crate::app::live_chat() {
                    chat.set_threads(threads);
                }
            }
            let held = crate::pages::threads_snapshot();
            eprintln!(
                "muse webui: {command} {verb} accepted={} threads={} ms={}",
                accepted,
                held.threads.len(),
                started.elapsed().as_millis()
            );
            serde_json::json!({
                "ok": true,
                "accepted": accepted,
                "thread": id,
                "verb": verb,
                "error": error,
                "reread_error": reread_error,
                "armed": crate::pages::thread_delete_armed(),
                "threads": held.threads,
                "total": held.total,
                "loaded": held.loaded,
            })
        }
        Fetched::Feed(held) => {
            crate::pages::feed_cache(held.clone());
            eprintln!(
                "muse webui: {command} read the service: cards={} total={} ms={}",
                held.cards.len(),
                held.total,
                started.elapsed().as_millis()
            );
            feed_reply(&held)
        }
        Fetched::Search(held) => {
            crate::pages::search_cache(held.clone());
            eprintln!(
                "muse webui: {command} read the service: rows={} total={} query={:?} ms={}",
                held.rows.len(),
                held.total,
                held.query,
                started.elapsed().as_millis()
            );
            search_reply(&held)
        }
        Fetched::Hero(data) => {
            eprintln!(
                "muse webui: {command} read the hero cache: cached={} ms={}",
                data.is_some(),
                started.elapsed().as_millis()
            );
            serde_json::json!({ "ok": true, "data": data })
        }
        Fetched::Failed(error) => {
            eprintln!("muse webui: {command} failed: {error}");
            serde_json::json!({ "ok": false, "error": error })
        }
    }
}

/// Hand one answer back to the page's promise.
fn send_reply(view: &webkit6::WebView, id: &serde_json::Value, reply: &serde_json::Value) {
    use webkit6::prelude::WebViewExt;
    let script = format!("window.museReply({id}, {reply});");
    view.evaluate_javascript(&script, None, None, gtk4::gio::Cancellable::NONE, |_| {});
}

/// Wait for a worker's answer on the main loop: one timed look every
/// [`ANSWER_POLL`], never an idle spin, so a 27 s listing costs a handful of
/// wakeups instead of a core.
fn await_answer(
    view: webkit6::WebView,
    id: serde_json::Value,
    receiver: Receiver<Fetched>,
    command: String,
    started: Instant,
) {
    let _ = gtk4::glib::timeout_add_local(ANSWER_POLL, move || {
        let fetched = match receiver.try_recv() {
            Ok(fetched) => fetched,
            Err(TryRecvError::Empty) => return gtk4::glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => return gtk4::glib::ControlFlow::Break,
        };
        let reply = reply_for(fetched, &command, started);
        send_reply(&view, &id, &reply);
        gtk4::glib::ControlFlow::Break
    });
}

thread_local! {
    /// The windows a page has open, if any: a native paint nudges every one of
    /// them by name. A list, not one entry, because a page can be opened twice
    /// and the older window must not go stale. Weak, so a window the reader
    /// closed simply stops upgrading and is dropped on the next nudge.
    static OPEN_WINDOWS: std::cell::RefCell<
        std::collections::HashMap<&'static str, Vec<gtk4::glib::WeakRef<webkit6::WebView>>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn track_window(page: &'static str, view: &webkit6::WebView) {
    use webkit6::prelude::*;
    let weak = view.downgrade();
    OPEN_WINDOWS.with(|windows| {
        windows.borrow_mut().entry(page).or_default().push(weak);
    });
}

/// Tell every open window for `page` that a native paint changed what it
/// shows. The page re-asks its own snapshot; nothing is pushed as data, so the
/// cache the paint wrote stays the only source of truth. Windows the reader
/// closed are pruned here, so the list cannot grow without bound.
pub(crate) fn nudge_page(page: &'static str, hook: &str) {
    OPEN_WINDOWS.with(|windows| {
        let mut windows = windows.borrow_mut();
        let Some(views) = windows.get_mut(page) else {
            return;
        };
        views.retain(|weak| {
            let Some(view) = weak.upgrade() else {
                return false;
            };
            use webkit6::prelude::*;
            let script = format!("window.{hook} && window.{hook}();");
            view.evaluate_javascript(&script, None, None, gtk4::gio::Cancellable::NONE, |_| {});
            true
        });
    });
}

/// Close every open window for `page`. The first-run step is one window's whole
/// life: when the app's setup returns, the window that showed it goes, the way
/// the modal did.
pub(crate) fn close_page_window(page: &'static str) {
    let open = OPEN_WINDOWS.with(|windows| windows.borrow_mut().remove(page).unwrap_or_default());
    for weak in open {
        let Some(view) = weak.upgrade() else {
            continue;
        };
        if let Some(root) = gtk4::prelude::WidgetExt::root(&view)
            && let Ok(window) = gtk4::prelude::Cast::downcast::<gtk4::Window>(root)
        {
            gtk4::prelude::GtkWindowExt::close(&window);
        }
    }
}

/// The updater's answer changed: an open Settings page should re-ask, since a
/// check it pressed lands after that press answered, and a check the native
/// card pressed lands without the page knowing. Same hook the policy read uses:
/// the page reads its own snapshot again instead of being handed data.
pub(crate) fn settings_updates_changed() {
    nudge_page("settings", "museUpdatesReady");
    // The shell runs every page in one window, so its Settings card is not one
    // of `nudge_page`'s: the nudge rides the pipe, and the window calls the
    // same hook by name. Without this the card a press started stays on its
    // busy step, because the answer lands after the press's own read.
    crate::web_shell::page_changed("settings");
}

/// The policy projection changed: an open Settings window should re-ask, since
/// the chat panel's read lands after that window opened.
pub(crate) fn settings_policy_changed() {
    nudge_page("settings", "musePolicyReady");
    // Same reason as the updater's nudge: the shell's Settings page is the
    // window's document, not a page window the app can evaluate into.
    crate::web_shell::page_changed("settings");
}

/// A Tasks read landed: the next run Upcoming names is that read's answer, so
/// an open Upcoming -- in the shell or in its own window -- re-asks its own
/// snapshot. Nothing is pushed as data, so the held cache stays the only
/// source, and a page that was showing "not read yet" stops saying it.
pub(crate) fn upcoming_changed() {
    nudge_page("upcoming", "museUpcomingReady");
    // The shell runs every page in one window, so its nudge rides the pipe
    // rather than `nudge_page`: the window calls the same hook, by name.
    crate::web_shell::page_changed("upcoming");
}

/// Open one page in its own window on the rows the native page last read.
/// Returns false when this build cannot render a WebView.
fn open_window(
    page: Option<&'static str>,
    title: &str,
    document: String,
    client: Option<AccountClient>,
    probe: &'static str,
) -> bool {
    open_window_with(page, title, document, client, probe, None)
}

/// The same window, with a page's route back into the app for its one
/// mutating command. Only Chat passes one.
fn open_window_with(
    page: Option<&'static str>,
    title: &str,
    document: String,
    client: Option<AccountClient>,
    probe: &'static str,
    routes: Option<WebRoutes>,
) -> bool {
    if !available() {
        return false;
    }
    use webkit6::prelude::*;
    use webkit6::{NavigationPolicyDecision, PolicyDecisionType};

    // Registered before the view exists, so the handler is in place when the
    // document's first script runs.
    let manager = webkit6::UserContentManager::new();
    if !manager.register_script_message_handler("muse", None) {
        eprintln!("muse webui: the command channel could not be registered");
        return false;
    }
    let view = webkit6::WebView::builder()
        .user_content_manager(&manager)
        .build();
    if let Some(page) = page {
        track_window(page, &view);
    }
    if let Some(settings) = WebViewExt::settings(&view) {
        settings.set_enable_javascript(true);
        settings.set_enable_developer_extras(false);
        // A local, static page gains nothing from the accelerated path, and
        // this is the one setting that keeps it visible on a seat with no
        // working GL (an Xvfb check, a VM, a broken driver). It also makes
        // the window capturable, which the accelerated surface is not.
        settings.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Never);
    }

    // Commands: parse, answer, hand the answer back on this same main loop.
    // Only the wire read leaves the loop, and it comes back through the timed
    // poll below, so no lock or widget is touched off the main thread.
    let reply_view = view.clone();
    // The probe's clock: how long the window took to render, and how long its
    // first command waited. Same numbers the Tauri window logs, so the two are
    // comparable rather than argued about.
    let probing = std::env::var_os("MUSE_PROBE").is_some();
    let first_command = std::rc::Rc::new(std::cell::Cell::new(true));
    let log_title = title.to_owned();
    // Named commands so far, per window: see `should_log_command`.
    let named: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new()));
    manager.connect_script_message_received(None, move |_, value| {
        let raw = value.to_str().to_string();
        let handling = Instant::now();
        let (id, answer, command) = handle_command_with(&raw, client.clone(), routes.as_ref());
        if probing && first_command.replace(false) {
            eprintln!(
                "muse webui: {} first command {command} at {}ms",
                log_title,
                since_start_ms()
            );
        }
        if should_log_command(&command, &mut named.borrow_mut()) {
            eprintln!("muse webui: command {command}");
        }
        if probing && matches!(answer, Answer::Now(_)) {
            eprintln!(
                "muse webui: {} answered {command} in {}us",
                log_title,
                handling.elapsed().as_micros()
            );
        }
        match answer {
            Answer::Now(reply) => send_reply(&reply_view, &id, &reply),
            Answer::Later(receiver) => {
                await_answer(reply_view.clone(), id, receiver, command, Instant::now())
            }
        }
    });

    // The same navigation lock the feed frames use: a toolbar or link cannot
    // turn this window into a browser. http(s) opens in the reader's own
    // browser on a user gesture; everything else is ignored, and the page
    // itself is not a navigation.
    view.connect_decide_policy(|_, decision, kind| {
        if kind != PolicyDecisionType::NavigationAction {
            return false;
        }
        let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() else {
            return false;
        };
        let attempted = navigation.navigation_action().and_then(|mut action| {
            let gesture = action.is_user_gesture();
            action
                .request()
                .map(|request| (request.uri().unwrap_or_default(), gesture))
        });
        if let Some((uri, gesture)) = attempted {
            if uri == "about:blank" {
                return false;
            }
            if gesture && (uri.starts_with("https://") || uri.starts_with("http://")) {
                let _ = gtk4::gio::AppInfo::launch_default_for_uri(
                    &uri,
                    None::<&gtk4::gio::AppLaunchContext>,
                );
            }
            decision.ignore();
            return true;
        }
        false
    });

    // Proof that the document rendered, independent of pixels, and that the
    // channel answered: the rows the page built and holds, and any script
    // error. Read after a beat so the React effects and the first command
    // round trip have finished.
    //
    // The probe must be a plain expression with a synchronous value.
    // `evaluate_javascript` does NOT await a returned promise: it answers
    // `Err("Unsupported result type")`, which the `if let Ok` below drops
    // silently, so a promise-shaped probe looks exactly like a probe that never
    // ran. For anything that needs a delay -- typing, clicking, waiting for a
    // reply -- evaluate in steps on separate `timeout_add_local_once` ticks
    // and keep each script synchronous. A probe written the async way cost a
    // debugging cycle while proving the composer's send.
    let loaded_title = title.to_owned();
    view.connect_load_changed(move |view, event| {
        if event != webkit6::LoadEvent::Finished {
            return;
        }
        if probing {
            eprintln!(
                "muse webui: {} document loaded at {}ms",
                loaded_title,
                since_start_ms()
            );
        }
        let probe_view = view.clone();
        gtk4::glib::timeout_add_local_once(Duration::from_millis(700), move || {
            probe_view.evaluate_javascript(
                probe,
                None,
                None,
                gtk4::gio::Cancellable::NONE,
                |result| {
                    if let Ok(value) = result {
                        eprintln!("muse webui: {}", value.to_string().trim_matches('"'));
                    }
                },
            );
        });
    });

    let window = gtk4::Window::builder()
        .title(title)
        .default_width(1040)
        .default_height(760)
        .build();
    window.set_child(Some(&view));
    view.load_html(&document, Some("about:blank"));
    window.present();
    true
}

const LIBRARY_PROBE: &str = "JSON.stringify({cards: document.querySelectorAll('.card').length, \
     rows: (window.__MUSE_ROWS__ || 0), \
     boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.artifacts || []).length, \
     err: window.__MUSE_ERR__ || '', \
     text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})";

const GOALS_PROBE: &str = "JSON.stringify({rows: (window.__MUSE_ROWS__ || 0), \
     cards: document.querySelectorAll('.goal').length, \
     boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.goals || []).length, \
     err: window.__MUSE_ERR__ || '', \
     text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})";

const TASKS_PROBE: &str = "JSON.stringify({rows: document.querySelectorAll('.task').length, \
     boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.schedules || []).length, \
     err: window.__MUSE_ERR__ || '', \
     text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})";

const IDEAS_PROBE: &str = "JSON.stringify({cards: document.querySelectorAll('.idea').length, \
     rows: (window.__MUSE_ROWS__ || 0), \
     boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.ideas || []).length, \
     err: window.__MUSE_ERR__ || '', \
     text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})";

const THREADS_PROBE: &str = "JSON.stringify({rows: document.querySelectorAll('.thread').length, \
     boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.threads || []).length, \
     err: window.__MUSE_ERR__ || '', \
     text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})";

/// Open the Library page in its own window, on the rows the native page last
/// read.
pub(crate) fn open_library_window(
    artifacts: Vec<ArtifactSummary>,
    total: u64,
    client: Option<AccountClient>,
) -> bool {
    open_window(
        None,
        "Library · web view",
        library_document(&artifacts, total),
        client,
        LIBRARY_PROBE,
    )
}

/// Open the Goals page in its own window, on the roster and Tracking rows the
/// native page last read.
pub(crate) fn open_goals_window(held: HeldGoals, client: Option<AccountClient>) -> bool {
    open_window(
        None,
        "Goals · web view",
        goals_document(&held),
        client,
        GOALS_PROBE,
    )
}

/// Open the Tasks page in its own window, on the schedules, runs and local
/// reminders the native page last read.
pub(crate) fn open_tasks_window(held: HeldTasks, client: Option<AccountClient>) -> bool {
    open_window(
        None,
        "Tasks · web view",
        tasks_document(&held),
        client,
        TASKS_PROBE,
    )
}

/// Open the Ideas page in its own window, on the roster the native page last
/// read.
pub(crate) fn open_ideas_window(held: HeldIdeas, client: Option<AccountClient>) -> bool {
    open_window(
        None,
        "Ideas · web view",
        ideas_document(&held),
        client,
        IDEAS_PROBE,
    )
}

/// Open the Threads page in its own window, on the rows the native page last
/// painted.
pub(crate) fn open_threads_window(held: HeldThreads, client: Option<AccountClient>) -> bool {
    open_window(
        None,
        "Threads · web view",
        threads_document(&held),
        client,
        THREADS_PROBE,
    )
}

const FEED_PROBE: &str = "JSON.stringify({cards: document.querySelectorAll('.feed-card').length, \
     graphics: document.querySelectorAll('.feed-graphic').length, \
     boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.cards || []).length, \
     err: window.__MUSE_ERR__ || '', \
     text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})";

/// Open the Feed page in its own window, on the cards the native page last
/// painted.
pub(crate) fn open_feed_window(held: HeldFeed, client: Option<AccountClient>) -> bool {
    open_window(
        None,
        "Feed · web view",
        feed_document(&held),
        client,
        FEED_PROBE,
    )
}

const LOCALHOST_PROBE: &str = "JSON.stringify({grants: document.querySelectorAll('.grant-row').length, \
                                    cards: document.querySelectorAll('.approval-card').length, \
                                    log: document.querySelectorAll('.log-line').length, \
                                    phase: ((document.querySelector('.host-phase') || {}).textContent || '').trim(), \
                                    device: ((document.querySelector('.host-device') || {}).textContent || '').trim().slice(0, 20)})";

const SHELL_PROBE: &str = "JSON.stringify({rail: document.querySelectorAll('.shell-rail-page').length, \
     drawer: document.querySelectorAll('.shell-drawer-page').length, \
     current: ((document.querySelector('.shell-drawer-page.current') || {}).textContent || '').trim(), \
     title: ((document.querySelector('.shell-main .title') || {}).textContent || '').trim()})";

/// Probes for the three surfaces: each reads a plain synchronous expression, so
/// the value is the page's own render and not a promise the view cannot await.
const SIGNIN_PROBE: &str = "JSON.stringify({fields: document.querySelectorAll('.signin-page input').length, \
     buttons: document.querySelectorAll('.signin-page .inline-button').length})";
const FIRST_RUN_PROBE: &str =
    "JSON.stringify({head: (document.querySelector('.firstrun-head') || {}).textContent || ''})";
const SETTINGS_PROBE: &str = "JSON.stringify({rows: document.querySelectorAll('.policy-row').length, \
                                 linked: (document.querySelector('.linked') ? true : false), \
                                 policy: (document.querySelector('.policy-loaded') ? true : false), \
                                 status: ((document.querySelector('.updates-status') || {}).textContent || '').slice(0, 40)})";

const SEARCH_PROBE: &str = "JSON.stringify({rows: document.querySelectorAll('.search-row').length, \
     boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.rows || []).length, \
     query: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.query) || '', \
     err: window.__MUSE_ERR__ || '', \
     text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})";

const CHAT_PROBE: &str = "JSON.stringify({rows: document.querySelectorAll('.chat-row').length, \
     boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.rows || []).length, \
     err: window.__MUSE_ERR__ || '', \
     text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})";

/// Open the Chat surface in its own window, on the rows the native transcript
/// last built. The page follows `chat.snapshot`, so it keeps up with the
/// app's own reads and with a reply that grows while it arrives.
pub(crate) fn open_chat_window(
    held: HeldChat,
    client: Option<AccountClient>,
    routes: WebRoutes,
) -> bool {
    open_window_with(
        None,
        "Chat · web view",
        chat_document(&held),
        client,
        CHAT_PROBE,
        Some(routes),
    )
}

/// Open the Search page in its own window, on the query and rows the native
/// search last painted.
pub(crate) fn open_search_window(held: HeldSearch, client: Option<AccountClient>) -> bool {
    open_window(
        None,
        "Search · web view",
        search_document(&held),
        client,
        SEARCH_PROBE,
    )
}

/// Open the Settings page in its own window, on the link, updater answer and
/// policy the native side holds. No client: this page reads nothing over the
/// wire, and the presses that would stay in the app's own controllers.
/// Open the web shell: the rail, the page list and every page in one window.
/// Each page asks its own snapshot there, so the shell's own payload is only
/// the list.
pub(crate) fn open_shell_window(routes: Option<WebRoutes>) -> bool {
    open_window_with(
        None,
        "Muse · web shell",
        shell_document(routes.as_ref()),
        None,
        SHELL_PROBE,
        routes,
    )
}

/// The sign-in surface in its own window: the same form the app builds, on the
/// app's own defaults and rules.
pub(crate) fn open_signin_window(routes: Option<WebRoutes>) -> bool {
    open_window_with(
        Some("signin"),
        "Sign in · web view",
        signin_document(),
        None,
        SIGNIN_PROBE,
        routes,
    )
}

/// The routes a non-chat surface gets: its own presses, and a refusal by name
/// for every press the chat window owns. A window that is not the chat's says
/// what it cannot do instead of silently doing nothing.
pub(crate) fn signin_surface(press: WebSigninPress) -> WebRoutes {
    fn no_send() -> Result<(), String> {
        Err("This window is the sign-in surface; it cannot send.".to_owned())
    }
    WebRoutes {
        send: std::rc::Rc::new(|_: &str, _: Option<WebQuote>| no_send()),
        stop: std::rc::Rc::new(no_send),
        attach: std::rc::Rc::new(no_send),
        unstage: std::rc::Rc::new(|_: &str| no_send()),
        approvals: std::rc::Rc::new(no_send),
        decide: std::rc::Rc::new(|_: &str, _: &str| no_send()),
        turn_delete: std::rc::Rc::new(no_send),
        draft: std::rc::Rc::new(|_: &str| no_send()),
        dictate: std::rc::Rc::new(no_send),
        history: std::rc::Rc::new(|| Err("This window cannot read the drawer.".to_owned())),
        open_thread: std::rc::Rc::new(|_: Option<&str>| no_send()),
        set_policy: std::rc::Rc::new(|_: &str, _: &str| no_send()),
        reset_connector: std::rc::Rc::new(|_: &str| no_send()),
        host_grant: std::rc::Rc::new(|_: &str, _: bool, _: bool| no_send()),
        host_start: std::rc::Rc::new(|_: bool| no_send()),
        host_stop: std::rc::Rc::new(no_send),
        host_decide: std::rc::Rc::new(|_: &str, _: bool| no_send()),
        host_policy: std::rc::Rc::new(|_: bool| no_send()),
        host_clear_log: std::rc::Rc::new(no_send),
        host_search: std::rc::Rc::new(|_: &str| no_send()),
        host_refresh: std::rc::Rc::new(no_send),
        host_content: std::rc::Rc::new(|_: &str| no_send()),
        host_preview: std::rc::Rc::new(|_: &str, _: &str| {
            Err("this window cannot read your folders".to_owned())
        }),
        update_check: std::rc::Rc::new(no_send),
        update_install: std::rc::Rc::new(no_send),
        signin_press: press,
        sign_out: std::rc::Rc::new(|| {
            Err("This window is the sign-in surface; it cannot sign out.".to_owned())
        }),
    }
}

/// The first-run step in its own window: two lines and a spinner, nothing to
/// press.
pub(crate) fn open_first_run_window() -> bool {
    open_window(
        Some("firstrun"),
        "Muse · first run",
        first_run_document(),
        None,
        FIRST_RUN_PROBE,
    )
}

pub(crate) fn open_settings_window(held: HeldSettings, routes: Option<WebRoutes>) -> bool {
    open_window_with(
        Some("settings"),
        "Settings · web view",
        settings_document(&held),
        None,
        SETTINGS_PROBE,
        routes,
    )
}

/// Open the Local host page in its own window. No client: the host's state is
/// held natively and the page reads none of it over the wire.
pub(crate) fn open_localhost_window(held: HeldLocalHost, routes: Option<WebRoutes>) -> bool {
    open_window_with(
        None,
        "Local host · web view",
        localhost_document(&held),
        None,
        LOCALHOST_PROBE,
        routes,
    )
}

/// Every route the Chat window has, stubbed: a test binds only the ones it is
/// about, so a new control cannot quietly break the others' tests. Also shared
/// with the app's own shell bridge tests, which answer pages with no display.
#[cfg(test)]
pub(crate) fn stub_routes() -> WebRoutes {
    use std::rc::Rc;
    WebRoutes {
        send: Rc::new(|_: &str, _: Option<WebQuote>| Ok(())),
        stop: Rc::new(|| Ok(())),
        attach: Rc::new(|| Ok(())),
        unstage: Rc::new(|_: &str| Ok(())),
        approvals: Rc::new(|| Ok(())),
        decide: Rc::new(|_: &str, _: &str| Ok(())),
        turn_delete: Rc::new(|| Ok(())),
        draft: Rc::new(|_: &str| Ok(())),
        dictate: Rc::new(|| Ok(())),
        history: Rc::new(|| Ok(Vec::new())),
        open_thread: Rc::new(|_: Option<&str>| Ok(())),
        set_policy: Rc::new(|_: &str, _: &str| Ok(())),
        reset_connector: Rc::new(|_: &str| Ok(())),
        host_grant: Rc::new(|_: &str, _: bool, _: bool| Ok(())),
        host_start: Rc::new(|_: bool| Ok(())),
        host_stop: Rc::new(|| Ok(())),
        host_decide: Rc::new(|_: &str, _: bool| Ok(())),
        host_policy: Rc::new(|_: bool| Ok(())),
        host_clear_log: Rc::new(|| Ok(())),
        host_search: Rc::new(|_: &str| Ok(())),
        host_refresh: Rc::new(|| Ok(())),
        host_content: Rc::new(|_: &str| Ok(())),
        host_preview: Rc::new(|_: &str, _: &str| Ok(serde_json::json!({}))),
        update_check: Rc::new(|| Ok(())),
        update_install: Rc::new(|| Ok(())),
        signin_press: Rc::new(|_: SigninPress| Ok(())),
        sign_out: Rc::new(|| Ok(())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_account::ListingItem;

    fn summary(title: &str, pinned: bool) -> ArtifactSummary {
        ArtifactSummary {
            artifact_ref: "ref".into(),
            title: title.into(),
            kind: Some("file".into()),
            format: Some("markdown".into()),
            status: None,
            placement: None,
            path: None,
            target_kind: Some("file".into()),
            slug: None,
            updated_at_ms: Some(1_700_000_000_000),
            pinned,
            can_open: true,
            can_share: false,
        }
    }

    fn goal(title: &str, kind: Option<&str>, body: Option<&str>) -> ListingItem {
        ListingItem {
            id: Some(format!("goal-{title}")),
            title: title.into(),
            body: body.map(str::to_owned),
            kind: kind.map(str::to_owned),
            ..ListingItem::default()
        }
    }

    fn task_row(id: &str, label: &str) -> crate::pages::TaskRow {
        crate::pages::TaskRow {
            id: id.into(),
            label: label.into(),
        }
    }

    fn held(goals: Vec<ListingItem>, tracking: Vec<ListingItem>) -> HeldGoals {
        HeldGoals {
            goals_total: goals.len() as u64,
            goals,
            tracking_total: tracking.len() as u64,
            tracking,
            tracking_error: None,
        }
    }

    /// The built page carries its stylesheet and script inline, and the rows
    /// arrive as the boot payload rather than as markup to scrape.
    #[test]
    fn the_document_carries_style_script_and_rows() {
        let doc = library_document(&[summary("Quarterly plan", true)], 76);
        assert!(doc.contains("--accent: #007ffd"), "stylesheet inlined");
        assert!(doc.contains("__MUSE_BOOT__"), "boot block present");
        assert!(doc.contains("Quarterly plan"), "rows inlined");
        assert!(doc.contains("\"total\":76"), "service total inlined");
        assert!(doc.contains("\"pinned\":true"));
        assert!(!doc.contains("/*__BOOT__*/"));
        assert!(!doc.contains("<script src="), "no fetched script");
        assert!(
            !doc.contains("<link rel=\"stylesheet\""),
            "no fetched stylesheet"
        );
    }

    #[test]
    fn a_title_cannot_close_the_data_block() {
        let doc = library_document(&[summary("</script><b>x", false)], 1);
        assert!(!doc.contains("</script><b>"));
        assert!(doc.contains("<\\/script><b>"));
    }

    /// One bundle serves every window: the boot payload names its page, and
    /// the goals window carries the roster, the Tracking rows and the
    /// Tracking failure beside it.
    #[test]
    fn the_goals_document_names_its_page_and_carries_both_sections() {
        let mut rows = held(
            vec![goal(
                "Ship the port",
                Some("in_progress"),
                Some("first line\nsecond"),
            )],
            vec![goal("Tracked work", None, None)],
        );
        rows.tracking_error = Some("tracking read timed out".into());
        let doc = goals_document(&rows);
        assert!(doc.contains("\"page\":\"goals\""), "page named in boot");
        assert!(doc.contains("Ship the port"));
        assert!(doc.contains("Tracked work"));
        assert!(doc.contains("\"goals_total\":1"));
        assert!(doc.contains("tracking read timed out"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    /// The cache read answers at once, for both pages, and refuses an
    /// unknown command by name instead of guessing.
    #[test]
    fn snapshots_answer_at_once_and_unknown_commands_are_refused() {
        let (id, answer, command) =
            handle_command_with(r#"{"id":7,"cmd":"library.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(7));
        assert_eq!(command, "library.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert!(reply.get("artifacts").is_some());
        assert!(reply.get("total").is_some());

        crate::pages::goals_cache(held(
            vec![goal("Held goal", Some("in_progress"), None)],
            vec![goal("Held tracking", None, None)],
        ));
        let (id, answer, command) =
            handle_command_with(r#"{"id":9,"cmd":"goals.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(9));
        assert_eq!(command, "goals.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["goals"][0]["title"], "Held goal");
        assert_eq!(reply["tracking"][0]["title"], "Held tracking");
        assert_eq!(reply["goals_total"], 1);

        let (_, answer, command) =
            handle_command_with(r#"{"id":8,"cmd":"library.delete"}"#, None, None);
        assert_eq!(command, "library.delete");
        let Answer::Now(reply) = answer else {
            panic!("an unknown command must be refused at once");
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "unknown command: library.delete");

        let (id, answer, _) = handle_command_with("not json", None, None);
        assert_eq!(id, serde_json::json!(0));
        let Answer::Now(reply) = answer else {
            panic!("a malformed request must be refused at once");
        };
        assert_eq!(reply["error"], "malformed request");
    }

    /// Refresh is a wire read, not the cache: it answers from a thread, and
    /// with no account link it says so instead of returning zero rows (which
    /// the page would have to describe as an empty library).
    #[test]
    fn refresh_reads_the_service_instead_of_the_held_rows() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-refresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let service = summary("Read from the service", false);
        let reply = serde_json::json!({
            "ok": true,
            "data": { "total_count": 128, "has_more": false, "artifacts": [service] }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("request");
            let mut writer = stream;
            writer.write_all(reply.as_bytes()).expect("reply");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
            line
        });

        // The held rows are deliberately different, so an answer that returns
        // them would prove the button re-read the cache.
        crate::pages::library_cache(vec![summary("Held, not re-read", false)], 1);
        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);

        let (_, answer, command) =
            handle_command_with(r#"{"id":3,"cmd":"library.refresh"}"#, Some(client), None);
        assert_eq!(command, "library.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        match receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers")
        {
            Fetched::Rows(artifacts, total) => {
                assert_eq!(artifacts.len(), 1);
                assert_eq!(artifacts[0].title, "Read from the service");
                assert_eq!(total, 128, "the service's total, not the page count");
            }
            Fetched::Failed(error) => panic!("the host answered: {error}"),
            Fetched::Goals(..) => panic!("a library read must not answer with goals"),
            Fetched::Tasks(..) => panic!("a library read must not answer with tasks"),
            Fetched::Ideas(..) => panic!("a library read must not answer with ideas"),
            Fetched::Threads(..) => panic!("a library read must not answer with threads"),
            Fetched::ThreadOp { .. } => panic!("a library read must not answer with a thread op"),
            Fetched::Feed(..) => panic!("a library read must not answer with a feed"),
            Fetched::Hero(..) => panic!("a library read must not answer with a hero"),
            Fetched::Search(..) => panic!("a library read must not answer with a search"),
            Fetched::Unshare { .. } => panic!("a library read must not answer with an unshare"),
            Fetched::SpaceDelete { .. } => panic!("a library read must not answer with a delete"),
            Fetched::SpaceExport { .. } => panic!("a library read must not answer with an export"),
            Fetched::TaskDelete { .. } => panic!("a library read must not answer with a delete"),
        }
        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"artifacts""#), "{request}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shell's own sequence for a page with no native paint behind it:
    /// the opening snapshot says nothing has been read, the page's refresh is
    /// the read that fills the rows, and the snapshot after it answers from
    /// those rows. Driven over a stub host socket, so the case costs no window
    /// and no service.
    #[test]
    fn an_unread_snapshot_then_the_pages_refresh_fills_the_rows() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        // Nothing has painted a listing into this process: the state a shell
        // launch starts every page but chat in.
        assert!(
            !crate::pages::library_loaded(),
            "a fresh thread holds no listing"
        );
        let (_, answer, command) =
            handle_command_with(r#"{"id":61,"cmd":"library.snapshot"}"#, None, None);
        assert_eq!(command, "library.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a snapshot is a cache read");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["loaded"], false, "nothing has been read yet");
        assert_eq!(reply["artifacts"], serde_json::json!([]));

        let dir = std::env::temp_dir().join(format!("muse-webui-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let reply_text = serde_json::json!({
            "ok": true,
            "data": {
                "total_count": 1,
                "has_more": false,
                "artifacts": [summary("Read by the page", false)],
            }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("request");
            let mut writer = stream;
            writer.write_all(reply_text.as_bytes()).expect("reply");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
            line
        });

        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);
        let (_, answer, command) =
            handle_command_with(r#"{"id":62,"cmd":"library.refresh"}"#, Some(client), None);
        assert_eq!(command, "library.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("the page's read leaves the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");
        let reply = reply_for(fetched, "library.refresh", Instant::now());
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["artifacts"][0]["title"], "Read by the page");
        assert_eq!(
            reply["loaded"], true,
            "the read is what makes the rows the page's"
        );
        assert!(crate::pages::library_loaded());
        // The read crossed the host once, as the listing: no second trip, and
        // no paint involved anywhere in the sequence.
        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"artifacts""#), "{request}");

        // The page's next open is answered from the read, not from "nothing".
        let (_, answer, _) =
            handle_command_with(r#"{"id":63,"cmd":"library.snapshot"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a snapshot is a cache read");
        };
        assert_eq!(reply["loaded"], true);
        assert_eq!(reply["artifacts"][0]["title"], "Read by the page");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_is_a_wire_read_and_reports_a_missing_link() {
        let (id, answer, command) =
            handle_command_with(r#"{"id":9,"cmd":"library.refresh"}"#, None, None);
        assert_eq!(id, serde_json::json!(9));
        assert_eq!(command, "library.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            Fetched::Rows(..) => panic!("no client, no rows"),
            Fetched::Goals(..) => panic!("no client, no goals"),
            Fetched::Tasks(..) => panic!("no client, no tasks"),
            Fetched::Ideas(..) => panic!("no client, no ideas"),
            Fetched::Threads(..) => panic!("no client, no threads"),
            Fetched::ThreadOp { .. } => panic!("no client, no thread change"),
            Fetched::Feed(..) => panic!("no client, no feed"),
            Fetched::Hero(..) => panic!("no client, no hero"),
            Fetched::Search(..) => panic!("no client, no search"),
            Fetched::Unshare { .. } => panic!("no client, no unshare"),
            Fetched::SpaceDelete { .. } => panic!("no client, no delete"),
            Fetched::SpaceExport { .. } => panic!("no client, no export"),
            Fetched::TaskDelete { .. } => panic!("no client, no schedule delete"),
        }
    }

    /// A schedule's delete is the app's two-press rule: the first press arms
    /// the row and writes nothing, a press on another row only re-arms, and
    /// only the second press on the same row starts the one delete. A listing
    /// painted again spends the arm.
    #[test]
    fn a_schedule_delete_arms_first_and_is_spent_by_a_second_press_or_a_repaint() {
        crate::pages::task_delete_forget();
        assert!(!crate::pages::task_delete_press("job-1"));
        assert!(
            crate::pages::task_delete_press("job-1"),
            "the second press confirms"
        );
        assert!(
            !crate::pages::task_delete_press("job-1"),
            "a confirm spends the arm"
        );
        assert!(
            !crate::pages::task_delete_press("job-2"),
            "a press on another row re-arms instead of confirming"
        );
        assert!(
            crate::pages::task_delete_press("job-2"),
            "the row armed last is the one a second press confirms"
        );
        crate::pages::task_delete_press("job-3");
        crate::pages::task_delete_forget();
        assert!(
            !crate::pages::task_delete_press("job-3"),
            "a listing painted again spends the arm"
        );
    }

    /// The route refuses a job the held listing does not name, and answers an
    /// armed first press from the held rows without touching the wire.
    #[test]
    fn a_schedule_delete_refuses_an_unknown_row_and_arms_a_known_one() {
        crate::pages::task_delete_forget();
        crate::pages::tasks_cache(crate::pages::HeldTasks {
            schedules: vec![crate::pages::TaskRow {
                id: "job-1".into(),
                label: "Nightly build (on)".into(),
            }],
            runs: Vec::new(),
            local: Vec::new(),
            next_run: None,
            loaded: true,
            error: None,
        });
        let (_, unknown, command) = handle_command_with(
            r#"{"id":70,"cmd":"tasks.delete","job":"job-9"}"#,
            None,
            None,
        );
        assert_eq!(command, "tasks.delete");
        match unknown {
            Answer::Now(value) => assert_eq!(value["ok"], serde_json::json!(false)),
            Answer::Later(_) => panic!("an unknown row must not start a delete"),
        }
        let (_, first, _) = handle_command_with(
            r#"{"id":71,"cmd":"tasks.delete","job":"job-1"}"#,
            None,
            None,
        );
        match first {
            Answer::Now(value) => {
                assert_eq!(value["ok"], serde_json::json!(true));
                assert_eq!(value["accepted"], serde_json::json!(false));
                assert_eq!(value["armed"], serde_json::json!("job-1"));
                assert_eq!(
                    value["schedules"][0]["label"],
                    serde_json::json!("Nightly build (on)")
                );
            }
            Answer::Later(_) => panic!("the arming press writes nothing, so it cannot read"),
        }
        crate::pages::task_delete_forget();
    }

    /// Opening a space is addressed by the held listing's own handle: a slug
    /// the listing does not carry is refused at once, and no export read is
    /// started for it.
    #[test]
    fn an_open_press_refuses_a_handle_the_listing_does_not_carry() {
        crate::pages::library_cache(Vec::new(), 0);
        let (_, answer, command) = handle_command_with(
            r#"{"id":91,"cmd":"library.open-space","slug":"not-held"}"#,
            None,
            None,
        );
        assert_eq!(command, "library.open-space");
        match answer {
            Answer::Now(value) => {
                assert_eq!(value["ok"], serde_json::json!(false));
                assert!(
                    value["error"].as_str().unwrap().contains("not in the held"),
                    "the refusal names the listing: {}",
                    value["error"]
                );
            }
            Answer::Later(_) => panic!("an unknown handle must not start an export"),
        }
    }

    /// A space's unshare arms before it writes: the first press leaves a
    /// confirmation owed and no request made, and a handle the held listing
    /// does not carry is refused instead of being sent.
    #[test]
    fn a_space_unshare_press_arms_first_and_refuses_an_unknown_handle() {
        let space = || muse_account::ArtifactSummary {
            artifact_ref: "space:calm-space".into(),
            title: "Calm space".into(),
            target_kind: Some("space".into()),
            slug: Some("calm-space".into()),
            can_share: true,
            ..muse_account::ArtifactSummary::default()
        };
        crate::pages::library_cache(vec![space()], 1);
        crate::pages::unshare_forget();

        let (_, answer, command) = handle_command_with(
            r#"{"id":31,"cmd":"library.unshare","slug":"calm-space"}"#,
            None,
            None,
        );
        assert_eq!(command, "library.unshare");
        let Answer::Now(reply) = answer else {
            panic!("the first press only arms: nothing leaves the app");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["accepted"], false);
        assert_eq!(reply["armed"], "calm-space");
        assert_eq!(reply["artifacts"][0]["title"], "Calm space");
        assert_eq!(crate::pages::unshare_armed().as_deref(), Some("calm-space"));

        // A handle the held rows do not carry never reaches the service, and
        // it does not disturb the arm either.
        let (_, answer, _) = handle_command_with(
            r#"{"id":32,"cmd":"library.unshare","slug":"not-here"}"#,
            None,
            None,
        );
        let Answer::Now(reply) = answer else {
            panic!("an unknown handle is refused at once");
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(crate::pages::unshare_armed().as_deref(), Some("calm-space"));
        crate::pages::unshare_forget();
    }

    /// An unshare's answer is the listing read after it: those rows are cached
    /// as the held ones, and the answer spends the arm -- acceptance by itself
    /// is never the change.
    #[test]
    fn an_unshare_answer_carries_the_re_read_listing() {
        let row = |title: &str| muse_account::ArtifactSummary {
            artifact_ref: "space:a".into(),
            title: title.into(),
            target_kind: Some("space".into()),
            slug: Some("a".into()),
            ..muse_account::ArtifactSummary::default()
        };
        crate::pages::library_cache(vec![row("Before")], 1);
        crate::pages::unshare_forget();
        assert!(!crate::pages::unshare_press("a"), "the press arms");

        let reply = reply_for(
            Fetched::Unshare {
                slug: "a".into(),
                accepted: true,
                error: None,
                listing: Some((vec![row("After")], 1)),
                reread_error: None,
            },
            "library.unshare",
            Instant::now(),
        );
        assert_eq!(reply["accepted"], true);
        assert_eq!(reply["armed"], serde_json::Value::Null);
        assert_eq!(reply["artifacts"][0]["title"], "After");
        assert_eq!(crate::pages::library_snapshot().0[0].title, "After");
        assert_eq!(crate::pages::unshare_armed(), None);
    }

    /// The confirm half of the unshare press, against a stub host: the second
    /// press sends exactly one path-only DELETE for the held handle, reads the
    /// listing again, and the rows the page paints are that read's, not the
    /// ones it held.
    #[test]
    fn a_confirmed_unshare_sends_one_path_only_delete_and_paints_the_re_read() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-unshare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");

        // The listing deliberately differs from the held row, so an answer
        // that painted the cache instead of the read would fail here.
        let after = serde_json::json!({
            "ok": true,
            "data": {
                "total_count": 1,
                "has_more": false,
                "artifacts": [{
                    "artifact_ref": "space:calm-space",
                    "title": "Read after the unshare",
                    "target_kind": "space",
                    "slug": "calm-space",
                    "pinned": false,
                    "can_open": false,
                    "can_share": true,
                }],
            }
        })
        .to_string();
        let accepted = serde_json::json!({
            "ok": true,
            "data": { "slug": "calm-space", "accepted": true }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            // Two calls, two connections: the DELETE, then the listing read.
            for reply in [accepted, after] {
                let (stream, _) = listener.accept().expect("accept");
                let mut line = String::new();
                BufReader::new(stream.try_clone().expect("clone"))
                    .read_line(&mut line)
                    .expect("request");
                seen.push(line);
                let mut writer = stream;
                writer.write_all(reply.as_bytes()).expect("reply");
                writer.write_all(b"\n").expect("newline");
                writer.flush().expect("flush");
            }
            seen
        });

        let held = |title: &str| muse_account::ArtifactSummary {
            artifact_ref: "space:calm-space".into(),
            title: title.into(),
            target_kind: Some("space".into()),
            slug: Some("calm-space".into()),
            ..muse_account::ArtifactSummary::default()
        };
        crate::pages::library_cache(vec![held("Held before the press")], 1);
        crate::pages::unshare_forget();

        // The first press only arms: no request leaves the app.
        let (_, answer, _) = handle_command_with(
            r#"{"id":41,"cmd":"library.unshare","slug":"calm-space"}"#,
            None,
            None,
        );
        assert!(
            matches!(answer, Answer::Now(_)),
            "the first press sends nothing"
        );
        assert_eq!(crate::pages::unshare_armed().as_deref(), Some("calm-space"));

        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);
        let (_, answer, command) = handle_command_with(
            r#"{"id":42,"cmd":"library.unshare","slug":"calm-space"}"#,
            Some(client),
            None,
        );
        assert_eq!(command, "library.unshare");
        let Answer::Later(receiver) = answer else {
            panic!("the confirm leaves the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");

        // What crossed the pipe to the host: one DELETE, whose whole payload
        // is the handle -- no path, no body, nothing that could aim it
        // elsewhere and no second call.
        let requests = server.join().expect("server");
        let sent: Vec<serde_json::Value> = requests
            .iter()
            .map(|line| serde_json::from_str(line.trim()).expect("one request per line"))
            .collect();
        let deletes: Vec<&serde_json::Value> = sent
            .iter()
            .filter(|request| request["op"] == "space_unshare")
            .collect();
        assert_eq!(deletes.len(), 1, "one unshare, never retried");
        assert_eq!(deletes[0]["slug"], "calm-space");
        let mut keys: Vec<&String> = deletes[0]
            .as_object()
            .expect("the request is an object")
            .keys()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            ["config_dir", "op", "slug"],
            "the handle and the app's own config dir, nothing else"
        );
        assert_eq!(
            sent.iter()
                .filter(|request| request["op"] == "artifacts")
                .count(),
            1,
            "the listing is read once"
        );
        assert_eq!(
            sent.iter()
                .map(|request| request["op"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>(),
            ["space_unshare", "artifacts"],
            "the DELETE goes first; the listing read is the proof after it"
        );

        // The rows in the answer are the read's: nothing is painted from the
        // held cache until that reply lands.
        assert_eq!(
            crate::pages::library_snapshot().0[0].title,
            "Held before the press"
        );
        let Fetched::Unshare {
            slug,
            accepted,
            error,
            listing,
            reread_error,
        } = fetched
        else {
            panic!("an unshare answers with its own variant");
        };
        assert_eq!(slug, "calm-space");
        assert!(accepted, "the stub accepted the DELETE");
        assert!(error.is_none(), "delete error: {error:?}");
        assert!(reread_error.is_none(), "re-read error: {reread_error:?}");
        let reply = reply_for(
            Fetched::Unshare {
                slug,
                accepted,
                error,
                listing,
                reread_error,
            },
            "library.unshare",
            Instant::now(),
        );
        assert_eq!(reply["accepted"], true);
        assert_eq!(reply["artifacts"][0]["title"], "Read after the unshare");
        assert_eq!(
            crate::pages::library_snapshot().0[0].title,
            "Read after the unshare"
        );
        assert_eq!(crate::pages::unshare_armed(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The delete's confirm half, against a stub host: the second press sends
    /// exactly one path-only DELETE, reads the listing again, and the reply
    /// paints the rows of that read -- with the deleted row absent from it.
    #[test]
    fn a_confirmed_delete_sends_one_path_only_delete_and_paints_the_re_read() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");

        // The read after the delete no longer lists the space.
        let after = serde_json::json!({
            "ok": true,
            "data": {
                "total_count": 1,
                "has_more": false,
                "artifacts": [{
                    "artifact_ref": "file:kept",
                    "title": "Kept file",
                    "target_kind": "file",
                    "pinned": false,
                    "can_open": true,
                    "can_share": false,
                }],
            }
        })
        .to_string();
        let accepted = serde_json::json!({
            "ok": true,
            "data": { "slug": "calm-space", "accepted": true }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in [accepted, after] {
                let (stream, _) = listener.accept().expect("accept");
                let mut line = String::new();
                BufReader::new(stream.try_clone().expect("clone"))
                    .read_line(&mut line)
                    .expect("request");
                seen.push(line);
                let mut writer = stream;
                writer.write_all(reply.as_bytes()).expect("reply");
                writer.write_all(b"\n").expect("newline");
                writer.flush().expect("flush");
            }
            seen
        });

        crate::pages::library_cache(
            vec![muse_account::ArtifactSummary {
                artifact_ref: "space:calm-space".into(),
                title: "Held space".into(),
                target_kind: Some("space".into()),
                slug: Some("calm-space".into()),
                ..muse_account::ArtifactSummary::default()
            }],
            1,
        );
        crate::pages::space_delete_forget();
        crate::pages::unshare_forget();

        let (_, answer, _) = handle_command_with(
            r#"{"id":51,"cmd":"library.delete-space","slug":"calm-space"}"#,
            None,
            None,
        );
        assert!(
            matches!(answer, Answer::Now(_)),
            "the first press sends nothing"
        );
        assert_eq!(
            crate::pages::space_delete_armed().as_deref(),
            Some("calm-space")
        );
        // The other write's arm is not moved by this one.
        assert_eq!(crate::pages::unshare_armed(), None);

        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);
        let (_, answer, command) = handle_command_with(
            r#"{"id":52,"cmd":"library.delete-space","slug":"calm-space"}"#,
            Some(client),
            None,
        );
        assert_eq!(command, "library.delete-space");
        let Answer::Later(receiver) = answer else {
            panic!("the confirm leaves the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");

        let requests = server.join().expect("server");
        let sent: Vec<serde_json::Value> = requests
            .iter()
            .map(|line| serde_json::from_str(line.trim()).expect("one request per line"))
            .collect();
        let deletes: Vec<&serde_json::Value> = sent
            .iter()
            .filter(|request| request["op"] == "space_delete")
            .collect();
        assert_eq!(deletes.len(), 1, "one delete, never retried");
        assert_eq!(deletes[0]["slug"], "calm-space");
        let mut keys: Vec<&String> = deletes[0]
            .as_object()
            .expect("the request is an object")
            .keys()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            ["config_dir", "op", "slug"],
            "the handle and the app's own config dir, nothing else"
        );
        assert_eq!(
            sent.iter()
                .map(|request| request["op"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>(),
            ["space_delete", "artifacts"],
            "the DELETE goes first; the listing read is the proof after it"
        );

        let Fetched::SpaceDelete {
            slug,
            accepted,
            error,
            listing,
            reread_error,
        } = fetched
        else {
            panic!("a delete answers with its own variant");
        };
        assert_eq!(slug, "calm-space");
        assert!(accepted, "the stub accepted the DELETE");
        assert!(error.is_none(), "delete error: {error:?}");
        assert!(reread_error.is_none(), "re-read error: {reread_error:?}");
        let reply = reply_for(
            Fetched::SpaceDelete {
                slug,
                accepted,
                error,
                listing,
                reread_error,
            },
            "library.delete-space",
            Instant::now(),
        );
        // The reply's rows are the read's: the space is gone from them.
        assert_eq!(reply["artifacts"][0]["title"], "Kept file");
        assert_eq!(reply["accepted"], true);
        let held = crate::pages::library_snapshot().0;
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].title, "Kept file");
        assert_eq!(crate::pages::space_delete_armed(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A refused DELETE still re-reads, and says both things: the service's
    /// refusal and the rows as they now are.
    #[test]
    fn a_refused_unshare_still_reports_the_held_listing() {
        let held = vec![muse_account::ArtifactSummary {
            artifact_ref: "space:a".into(),
            title: "Still shared".into(),
            target_kind: Some("space".into()),
            slug: Some("a".into()),
            ..muse_account::ArtifactSummary::default()
        }];
        crate::pages::library_cache(held, 1);
        let reply = reply_for(
            Fetched::Unshare {
                slug: "a".into(),
                accepted: false,
                error: Some("invalid_request".into()),
                listing: None,
                reread_error: Some("timeout".into()),
            },
            "library.unshare",
            Instant::now(),
        );
        assert_eq!(reply["accepted"], false);
        assert_eq!(reply["error"], "invalid_request");
        assert_eq!(reply["reread_error"], "timeout");
        assert_eq!(reply["artifacts"][0]["title"], "Still shared");
    }

    /// Goals refresh reads both sections on one thread, keeps the roster when
    /// only Tracking fails, and leaves the cache telling the same story.
    #[test]
    fn goals_refresh_reads_both_sections_and_caches_them() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-goals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let goals_reply = serde_json::json!({
            "ok": true,
            "data": {
                "total": 40, "recognized": 1, "skipped": 0,
                "items": [{ "title": "Read from the service", "kind": "in_progress" }]
            }
        })
        .to_string();
        let tracking_reply = serde_json::json!({
            "ok": true,
            "data": {
                "total": 3, "recognized": 1, "skipped": 0,
                "items": [{ "title": "Tracked from the service" }]
            }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let mut ops = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().expect("accept");
                let mut line = String::new();
                BufReader::new(stream.try_clone().expect("clone"))
                    .read_line(&mut line)
                    .expect("request");
                let reply = if line.contains(r#""op":"goals_tracking""#) {
                    &tracking_reply
                } else {
                    &goals_reply
                };
                let mut writer = stream;
                writer.write_all(reply.as_bytes()).expect("reply");
                writer.write_all(b"\n").expect("newline");
                writer.flush().expect("flush");
                ops.push(line);
            }
            ops
        });

        // Held rows deliberately differ from the service's answer.
        crate::pages::goals_cache(held(vec![goal("Held goal", None, None)], Vec::new()));
        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);

        let (_, answer, command) =
            handle_command_with(r#"{"id":12,"cmd":"goals.refresh"}"#, Some(client), None);
        assert_eq!(command, "goals.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");
        let Fetched::Goals(rows) = fetched else {
            panic!("the host answered with something else");
        };
        assert_eq!(rows.goals[0].title, "Read from the service");
        assert_eq!(rows.goals_total, 40);
        assert_eq!(rows.tracking[0].title, "Tracked from the service");
        assert_eq!(rows.tracking_total, 3);
        assert!(rows.tracking_error.is_none());

        let reply = reply_for(Fetched::Goals(rows), &command, Instant::now());
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["goals_total"], 40);
        assert_eq!(reply["loaded"], true, "the reply says the roster was read");
        let cached = crate::pages::goals_snapshot();
        assert_eq!(cached.goals[0].title, "Read from the service");
        assert_eq!(cached.tracking[0].title, "Tracked from the service");

        let requests = server.join().expect("server");
        assert!(requests[0].contains(r#""op":"goals""#), "{}", requests[0]);
        assert!(
            requests[1].contains(r#""op":"goals_tracking""#),
            "{}",
            requests[1]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn goals_refresh_without_a_client_reports_the_missing_link() {
        let (_, answer, command) =
            handle_command_with(r#"{"id":13,"cmd":"goals.refresh"}"#, None, None);
        assert_eq!(command, "goals.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no rows"),
        }
    }

    /// A worker's answer becomes the page's reply, and a failure stays a
    /// failure instead of an empty page.
    #[test]
    fn the_reply_reports_failures_by_name() {
        let reply = reply_for(
            Fetched::Failed("no account link in this app".into()),
            "goals.refresh",
            Instant::now(),
        );
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "no account link in this app");
    }

    /// One failed Tracking read must not empty a section the native page
    /// still shows: the held rows stay in the cache and in the reply, and
    /// the failure rides beside them.
    #[test]
    fn a_failed_tracking_read_keeps_the_rows_the_page_still_shows() {
        crate::pages::goals_cache(held(
            vec![goal("Held goal", None, None)],
            vec![
                goal("Held tracking one", None, None),
                goal("Held tracking two", None, None),
            ],
        ));

        // The worker could read the roster but not Tracking: its answer
        // carries the fresh goals and no tracking rows.
        let fetched = Fetched::Goals(HeldGoals {
            goals: vec![goal("Fresh goal", None, None)],
            goals_total: 7,
            tracking: Vec::new(),
            tracking_total: 0,
            tracking_error: Some("tracking read timed out".into()),
        });
        let reply = reply_for(fetched, "goals.refresh", Instant::now());

        assert_eq!(reply["ok"], true);
        assert_eq!(reply["goals"][0]["title"], "Fresh goal");
        assert_eq!(reply["tracking_error"], "tracking read timed out");
        assert_eq!(reply["tracking"][0]["title"], "Held tracking one");
        assert_eq!(reply["tracking"][1]["title"], "Held tracking two");
        assert_eq!(reply["tracking_total"], 2, "the held total, not zero");

        let cached = crate::pages::goals_snapshot();
        assert_eq!(cached.goals[0].title, "Fresh goal", "the roster is fresh");
        assert_eq!(
            cached.tracking.len(),
            2,
            "the cache keeps the rows the native page still shows"
        );
        assert_eq!(cached.tracking_total, 2);
        assert_eq!(
            cached.tracking_error.as_deref(),
            Some("tracking read timed out")
        );
    }

    /// Tasks boot: one page-tagged payload with the native page's own labels,
    /// including the difference between "not read yet" and "empty".
    #[test]
    fn the_tasks_document_names_its_page_and_carries_all_three_sections() {
        let rows = HeldTasks {
            schedules: vec![task_row("job-1", "Nightly build (on)")],
            runs: vec![task_row("run-1", "ok: built")],
            local: vec![task_row("7", "Call the dentist · in 2h · pending")],
            next_run: None,
            loaded: true,
            error: None,
        };
        let doc = tasks_document(&rows);
        assert!(doc.contains("\"page\":\"tasks\""), "page named in boot");
        assert!(doc.contains("Nightly build (on)"));
        assert!(doc.contains("ok: built"));
        assert!(doc.contains("Call the dentist"));
        assert!(doc.contains("\"loaded\":true"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    #[test]
    fn tasks_snapshot_answers_from_the_held_rows() {
        crate::pages::tasks_cache(HeldTasks {
            schedules: vec![task_row("job-1", "Nightly build (on)")],
            runs: vec![task_row("run-1", "failed")],
            local: Vec::new(),
            next_run: None,
            loaded: true,
            error: None,
        });
        let (id, answer, command) =
            handle_command_with(r#"{"id":21,"cmd":"tasks.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(21));
        assert_eq!(command, "tasks.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["schedules"][0]["label"], "Nightly build (on)");
        assert_eq!(reply["runs"][0]["label"], "failed");
        assert_eq!(reply["loaded"], true);
    }

    /// Tasks refresh reads the service on a thread and keeps the native
    /// page's words for both service sections.
    #[test]
    fn tasks_refresh_reads_the_service() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let service = serde_json::json!({
            "ok": true,
            "data": {
                "schedules": [{
                    "id": "job-1", "title": "Nightly build", "enabled": true,
                    "next_run_at_utc": null
                }],
                "runs": [{
                    "run_id": "run-1", "job_id": "job-1", "status": "ok",
                    "result_summary": "built"
                }],
                "agents": []
            }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("request");
            let mut writer = stream;
            writer.write_all(service.as_bytes()).expect("reply");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
            line
        });

        crate::pages::tasks_cache(HeldTasks {
            schedules: vec![task_row("held", "Held, not re-read (on)")],
            loaded: true,
            ..HeldTasks::default()
        });
        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);

        let (_, answer, command) =
            handle_command_with(r#"{"id":22,"cmd":"tasks.refresh"}"#, Some(client), None);
        assert_eq!(command, "tasks.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");
        let Fetched::Tasks(rows) = fetched else {
            panic!("the host answered with something else");
        };
        assert_eq!(rows.schedules[0].label, "Nightly build (on)");
        assert_eq!(rows.runs[0].label, "ok: built");
        assert!(rows.loaded);

        let reply = reply_for(Fetched::Tasks(rows), &command, Instant::now());
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["schedules"][0]["id"], "job-1");
        let cached = crate::pages::tasks_snapshot();
        assert_eq!(cached.schedules[0].label, "Nightly build (on)");
        assert!(cached.loaded);

        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"tasks""#), "{request}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tasks_refresh_without_a_client_reports_the_missing_link() {
        let (_, answer, command) =
            handle_command_with(r#"{"id":23,"cmd":"tasks.refresh"}"#, None, None);
        assert_eq!(command, "tasks.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no rows"),
        }
    }

    /// Ideas boot: one page-tagged payload with the roster and the
    /// not-loaded/loaded distinction.
    #[test]
    fn the_ideas_document_names_its_page_and_carries_the_roster() {
        let held = HeldIdeas {
            ideas: vec![goal("A calmer inbox", Some("Work"), Some("digest only"))],
            total: 9,
            loaded: true,
            error: None,
        };
        let doc = ideas_document(&held);
        assert!(doc.contains("\"page\":\"ideas\""), "page named in boot");
        assert!(doc.contains("A calmer inbox"));
        assert!(doc.contains("\"total\":9"));
        assert!(doc.contains("\"loaded\":true"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    #[test]
    fn ideas_snapshot_answers_from_the_held_rows() {
        crate::pages::ideas_cache(HeldIdeas {
            ideas: vec![goal("Held idea", None, None)],
            total: 3,
            loaded: true,
            error: None,
        });
        let (id, answer, command) =
            handle_command_with(r#"{"id":31,"cmd":"ideas.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(31));
        assert_eq!(command, "ideas.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["ideas"][0]["title"], "Held idea");
        assert_eq!(reply["total"], 3);
        assert_eq!(reply["loaded"], true);
    }

    /// Ideas refresh reads the service on a thread and updates the cache.
    #[test]
    fn ideas_refresh_reads_the_service() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-ideas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let service = serde_json::json!({
            "ok": true,
            "data": {
                "total": 12, "recognized": 1, "skipped": 0,
                "items": [{ "title": "Read from the service", "kind": "idea" }]
            }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("request");
            let mut writer = stream;
            writer.write_all(service.as_bytes()).expect("reply");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
            line
        });

        crate::pages::ideas_cache(HeldIdeas {
            ideas: vec![goal("Held, not re-read", None, None)],
            total: 1,
            loaded: true,
            error: None,
        });
        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);

        let (_, answer, command) =
            handle_command_with(r#"{"id":32,"cmd":"ideas.refresh"}"#, Some(client), None);
        assert_eq!(command, "ideas.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");
        let Fetched::Ideas(rows) = fetched else {
            panic!("the host answered with something else");
        };
        assert_eq!(rows.ideas[0].title, "Read from the service");
        assert_eq!(rows.total, 12);
        assert!(rows.loaded);

        let reply = reply_for(Fetched::Ideas(rows), &command, Instant::now());
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["ideas"][0]["title"], "Read from the service");
        let cached = crate::pages::ideas_snapshot();
        assert_eq!(cached.ideas[0].title, "Read from the service");
        assert_eq!(cached.total, 12);

        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"ideas""#), "{request}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ideas_refresh_without_a_client_reports_the_missing_link() {
        let (_, answer, command) =
            handle_command_with(r#"{"id":33,"cmd":"ideas.refresh"}"#, None, None);
        assert_eq!(command, "ideas.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no rows"),
        }
    }

    /// The wait is a timer, not an idle handler: an idle source would run the
    /// poll back to back and hold a core for a 27 s listing.
    #[test]
    fn the_answer_wait_is_a_timed_poll() {
        assert!(ANSWER_POLL >= Duration::from_millis(50));
        assert!(ANSWER_POLL <= Duration::from_millis(500));
    }

    /// A press on the sign-in page is the app's own validation, not a second
    /// set of rules: an empty interpreter is refused in the native form's
    /// words, and nothing is saved.
    #[test]
    fn a_signin_press_is_the_apps_own_validation() {
        let routes = signin_surface(crate::web_signin::routes(|| {}));
        let (_, answer, command) = handle_command_with(
            r#"{"id":31,"cmd":"signin.press","press":"connect","python":"","folder":"","consent":true}"#,
            None,
            Some(&routes),
        );
        assert_eq!(command, "signin.press");
        let Answer::Now(reply) = answer else {
            panic!("a press answers at once, with the route's own sentence");
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "Enter the Python interpreter path.");
    }

    /// The sign-in window is that surface and no other: a chat press in it is
    /// refused by name instead of quietly doing nothing.
    #[test]
    fn the_signin_window_refuses_a_send_by_name() {
        let routes = signin_surface(crate::web_signin::routes(|| {}));
        let (_, answer, _) = handle_command_with(
            r#"{"id":32,"cmd":"chat.send","text":"hello"}"#,
            None,
            Some(&routes),
        );
        let Answer::Now(reply) = answer else {
            panic!("a send's refusal answers at once");
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(
            reply["error"],
            "This window is the sign-in surface; it cannot send."
        );
    }

    /// The first-run page reads the app's own step: the same two sentences the
    /// native modal prints, and no presses at all.
    #[test]
    fn the_first_run_page_reads_the_apps_own_step() {
        crate::pages::first_run_cache(crate::pages::held_first_run(false));
        let (_, answer, _) =
            handle_command_with(r#"{"id":33,"cmd":"firstrun.snapshot"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a cache read answers without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["head"], crate::pages::FIRST_RUN_HEAD);
        assert_eq!(reply["sub"], crate::pages::FIRST_RUN_SUB);
        assert_eq!(reply["done"], false);

        crate::pages::first_run_cache(crate::pages::held_first_run(true));
        let (_, answer, _) =
            handle_command_with(r#"{"id":34,"cmd":"firstrun.snapshot"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a cache read answers without a thread");
        };
        assert_eq!(reply["done"], true);
    }

    #[test]
    fn the_chat_document_names_its_page_and_carries_the_rows() {
        let held = HeldChat {
            thread: Some("s-1".into()),
            title: "A calmer inbox".into(),
            account: "Muse".into(),
            status: Some("Send failed (Timeout).".into()),
            sending: false,
            rows: vec![
                crate::pages::ChatRow {
                    kind: "day",
                    day: "Today".into(),
                    ..crate::pages::ChatRow::default()
                },
                crate::pages::ChatRow {
                    kind: "message",
                    id: "m-1".into(),
                    role: "assistant".into(),
                    text: "**Hello** there".into(),
                    ..crate::pages::ChatRow::default()
                },
                crate::pages::ChatRow {
                    kind: "notice",
                    text: "Usage limit reached".into(),
                    ..crate::pages::ChatRow::default()
                },
            ],
            loaded: true,
            error: None,
            ..HeldChat::default()
        };
        let doc = chat_document(&held);
        assert!(doc.contains("\"page\":\"chat\""), "page named in boot");
        assert!(doc.contains("A calmer inbox"));
        assert!(doc.contains("\"kind\":\"day\""));
        assert!(doc.contains("Usage limit reached"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    /// A mirror page polls its cache, so the log names each cache command
    /// once per window; a wire read is named every time, because that is what
    /// the launch log is for. Without this, one chat window left open wrote a
    /// line every two seconds.
    #[test]
    fn the_log_names_cache_reads_once_and_wire_reads_every_time() {
        use std::collections::HashSet;
        let mut named = HashSet::new();
        assert!(should_log_command("chat.snapshot", &mut named));
        for _ in 0..200 {
            assert!(!should_log_command("chat.snapshot", &mut named));
        }
        // Another window has its own set, so it still names its first read.
        let mut other = HashSet::new();
        assert!(should_log_command("chat.snapshot", &mut other));
        // A Refresh press goes to the service: never swallowed.
        for _ in 0..3 {
            assert!(should_log_command("library.refresh", &mut named));
        }
        assert!(should_log_command("search.run", &mut named));
        assert!(should_log_command("search.run", &mut named));
        // A send and a stop go out: two presses are two lines.
        assert!(should_log_command("chat.send", &mut named));
        assert!(should_log_command("chat.send", &mut named));
        assert!(should_log_command("chat.stop", &mut named));
        // A decision is a wire write: every press is named, even a repeat.
        assert!(should_log_command("chat.decide", &mut named));
        assert!(should_log_command("chat.decide", &mut named));
        // The approvals re-read is a read that leaves the loop.
        assert!(should_log_command("chat.approvals.refresh", &mut named));
        assert!(should_log_command("chat.approvals.refresh", &mut named));
        assert!(should_log_command("chat.stop", &mut named));
        // A cache read asked twice is named once, but a different one is
        // still new to the log.
        assert!(should_log_command("threads.snapshot", &mut named));
        assert!(!should_log_command("threads.snapshot", &mut named));
    }

    /// The one mutating command dispatches nothing here: the draft goes back
    /// to the app's own send path, and the answer says whether that path took
    /// it. A refusal travels as the app's own sentence, and a window opened
    /// without the hook cannot send at all -- so there is exactly one sender.
    #[test]
    fn chat_send_hands_the_draft_to_the_app_and_reports_a_refusal() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = seen.clone();
        let quotes: Rc<RefCell<Vec<Option<WebQuote>>>> = Rc::new(RefCell::new(Vec::new()));
        let seen_quotes = quotes.clone();
        let send: WebSend = Rc::new(move |text: &str, quote| {
            recorded.borrow_mut().push(text.to_owned());
            seen_quotes.borrow_mut().push(quote);
            Ok(())
        });
        let routes = WebRoutes {
            send,
            stop: Rc::new(|| Ok(())),
            ..stub_routes()
        };
        let (id, answer, command) = handle_command_with(
            r#"{"id":61,"cmd":"chat.send","text":"hello there"}"#,
            None,
            Some(&routes),
        );
        assert_eq!(id, serde_json::json!(61));
        assert_eq!(command, "chat.send");
        let Answer::Now(reply) = answer else {
            panic!("a send answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(seen.borrow().as_slice(), ["hello there"]);
        assert_eq!(quotes.borrow().as_slice(), [None::<WebQuote>]);

        // A staged quote rides the same command, as data: the page says which
        // turn it answers, the app's own guard decides whether it is quotable.
        let (_, answer, _) = handle_command_with(
            r#"{"id":64,"cmd":"chat.send","text":"answering","quote":{"id":"m-9","excerpt":"hello"}}"#,
            None,
            Some(&routes),
        );
        let Answer::Now(reply) = answer else {
            panic!("a quoted send answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(
            quotes.borrow().last().cloned().flatten(),
            Some(WebQuote {
                id: "m-9".to_owned(),
                excerpt: "hello".to_owned(),
            })
        );

        let refusing = WebRoutes {
            send: Rc::new(|_: &str, _: Option<WebQuote>| {
                Err("Still working — your message is kept; send again in a moment.".to_owned())
            }),
            stop: Rc::new(|| Ok(())),
            ..stub_routes()
        };
        let (_, answer, _) = handle_command_with(
            r#"{"id":62,"cmd":"chat.send","text":"again"}"#,
            None,
            Some(&refusing),
        );
        let Answer::Now(reply) = answer else {
            panic!("a refusal answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert!(
            reply["error"]
                .as_str()
                .unwrap_or_default()
                .contains("message is kept")
        );

        // A page with no hook has no route to the service.
        let (_, answer, _) =
            handle_command_with(r#"{"id":63,"cmd":"chat.send","text":"hi"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a closed route answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["error"], serde_json::json!("this window cannot send"));
    }

    /// The composer's two shared controls: typing writes the app's own draft,
    /// and the mic is the app's own toggle. Neither has a second path, and a
    /// window without routes says it cannot make the change.
    #[test]
    fn chat_draft_and_dictate_are_the_apps_own() {
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        let written: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let seen = written.clone();
        let presses = Rc::new(Cell::new(0));
        let counted = presses.clone();
        let routes = WebRoutes {
            draft: Rc::new(move |text: &str| {
                seen.borrow_mut().push(text.to_owned());
                Ok(())
            }),
            dictate: Rc::new(move || {
                counted.set(counted.get() + 1);
                Ok(())
            }),
            ..stub_routes()
        };

        let (id, answer, command) = handle_command_with(
            r#"{"id":91,"cmd":"chat.draft","text":"hello"}"#,
            None,
            Some(&routes),
        );
        assert_eq!(id, serde_json::json!(91));
        assert_eq!(command, "chat.draft");
        let Answer::Now(reply) = answer else {
            panic!("a draft answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(written.borrow().as_slice(), ["hello"]);

        let (_, answer, _) =
            handle_command_with(r#"{"id":92,"cmd":"chat.dictate"}"#, None, Some(&routes));
        let Answer::Now(reply) = answer else {
            panic!("a mic press answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(presses.get(), 1);

        let (_, answer, _) = handle_command_with(r#"{"id":93,"cmd":"chat.dictate"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a closed route answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(
            reply["error"],
            serde_json::json!("this window cannot make that change")
        );
    }

    /// The turn menu's Delete is the app's own armed press, not a second
    /// delete path: the route dispatches once, the app's sentence travels back
    /// when it refuses, and a window without routes says it cannot delete
    /// instead of doing nothing.
    #[test]
    fn chat_delete_is_the_apps_own_press_and_refuses_a_closed_window() {
        use std::cell::Cell;
        use std::rc::Rc;
        let pressed = Rc::new(Cell::new(0));
        let counted = pressed.clone();
        let routes = WebRoutes {
            turn_delete: Rc::new(move || {
                counted.set(counted.get() + 1);
                Ok(())
            }),
            ..stub_routes()
        };
        let (id, answer, command) =
            handle_command_with(r#"{"id":81,"cmd":"chat.delete"}"#, None, Some(&routes));
        assert_eq!(id, serde_json::json!(81));
        assert_eq!(command, "chat.delete");
        let Answer::Now(reply) = answer else {
            panic!("a delete answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(pressed.get(), 1);

        // A press the app cannot take answers with the app's own sentence, so
        // the page can print it beside the press instead of guessing.
        let refusing = WebRoutes {
            turn_delete: Rc::new(|| Err("Another send is already in flight.".to_owned())),
            ..stub_routes()
        };
        let (_, answer, _) =
            handle_command_with(r#"{"id":82,"cmd":"chat.delete"}"#, None, Some(&refusing));
        let Answer::Now(reply) = answer else {
            panic!("a refusal answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(
            reply["error"],
            serde_json::json!("Another send is already in flight.")
        );

        // A page with no hook has no route to the delete at all.
        let (_, answer, _) = handle_command_with(r#"{"id":83,"cmd":"chat.delete"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a closed route answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(
            reply["error"],
            serde_json::json!("this window cannot make that change")
        );
    }

    /// Stop is the other control the native card has. Its route takes no
    /// ticket, so a press while a send holds the `Send` slot still goes out:
    /// here that is proven by the send route being held (it never answers) and
    /// the stop route still being reached.
    #[test]
    fn chat_stop_fires_its_own_route_and_refuses_a_closed_one() {
        use std::cell::Cell;
        use std::rc::Rc;
        let stopped = Rc::new(Cell::new(0));
        let counted = stopped.clone();
        let refusing = WebRoutes {
            send: Rc::new(|_: &str, _: Option<WebQuote>| {
                Err("Still working — your message is kept.".to_owned())
            }),
            stop: Rc::new(move || {
                counted.set(counted.get() + 1);
                Ok(())
            }),
            ..stub_routes()
        };
        let (id, answer, command) =
            handle_command_with(r#"{"id":71,"cmd":"chat.stop"}"#, None, Some(&refusing));
        assert_eq!(id, serde_json::json!(71));
        assert_eq!(command, "chat.stop");
        let Answer::Now(reply) = answer else {
            panic!("a stop answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(stopped.get(), 1);
        // The send route still refuses: stop is not a way around a held slot,
        // it is a way to cancel what holds it.
        let (_, answer, _) = handle_command_with(
            r#"{"id":72,"cmd":"chat.send","text":"not yet"}"#,
            None,
            Some(&refusing),
        );
        let Answer::Now(reply) = answer else {
            panic!("a refused send answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(stopped.get(), 1);

        // A window without the routes cannot stop on the page's say-so.
        let (_, answer, _) = handle_command_with(r#"{"id":73,"cmd":"chat.stop"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a closed route answers at once")
        };
        assert_eq!(reply["error"], serde_json::json!("this window cannot stop"));
    }

    /// A send outcome, a stop or a dictation's refusal is one line. The page
    /// reads it from the notice's own store, so saying it never rebuilds the
    /// transcript the page is already holding.
    #[test]
    fn a_note_reaches_the_page_without_rebuilding_the_transcript() {
        crate::pages::chat_cache(HeldChat {
            thread: Some("s-4".into()),
            title: "Held".into(),
            account: "Muse".into(),
            rows: vec![crate::pages::ChatRow {
                kind: "message",
                id: "m-4".into(),
                role: "assistant".into(),
                text: "Held reply".into(),
                ..crate::pages::ChatRow::default()
            }],
            loaded: true,
            error: None,
            ..HeldChat::default()
        });
        crate::pages::status_cache(None);
        let (_, answer, _) = handle_command_with(r#"{"id":61,"cmd":"chat.snapshot"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a snapshot answers at once")
        };
        assert_eq!(reply["status"], serde_json::json!(null));
        assert_eq!(reply["rows"].as_array().map(Vec::len), Some(1));

        crate::pages::status_cache(Some("Send failed (Timeout).".into()));
        let (_, answer, _) = handle_command_with(r#"{"id":62,"cmd":"chat.snapshot"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a snapshot answers at once")
        };
        assert_eq!(reply["status"], serde_json::json!("Send failed (Timeout)."));
        assert_eq!(reply["rows"][0]["id"], serde_json::json!("m-4"));
        crate::pages::status_cache(None);
    }

    /// The page follows the app: `chat.snapshot` answers from the held
    /// transcript without a wire trip, and a grown reply is in the answer, so
    /// a poll while a reply streams sees the words arrive.
    #[test]
    fn chat_snapshot_answers_the_held_transcript_and_follows_growth() {
        crate::pages::chat_cache(HeldChat {
            thread: Some("s-2".into()),
            title: "Streaming".into(),
            account: "Muse".into(),
            status: None,
            sending: true,
            rows: vec![crate::pages::ChatRow {
                kind: "message",
                id: "m-9".into(),
                role: "assistant".into(),
                text: "Hel".into(),
                ..crate::pages::ChatRow::default()
            }],
            loaded: true,
            error: None,
            ..HeldChat::default()
        });
        let (id, answer, command) =
            handle_command_with(r#"{"id":51,"cmd":"chat.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(51));
        assert_eq!(command, "chat.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["rows"][0]["text"], "Hel");
        assert_eq!(reply["title"], "Streaming");

        let mut grown = crate::pages::chat_snapshot();
        grown.rows[0].text = "Hello there, arriving in pieces.".into();
        crate::pages::chat_cache(grown);
        let (_, answer, _) = handle_command_with(r#"{"id":52,"cmd":"chat.snapshot"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["rows"][0]["text"], "Hello there, arriving in pieces.");
        assert_eq!(reply["rows"][0]["id"], "m-9", "the same turn, grown");
    }

    /// The three things the page cannot read for itself arrive in the same
    /// snapshot as the transcript: the picture staged in the app's composer,
    /// the open thread's unread count, and the pending approvals.
    #[test]
    fn the_chat_page_gets_the_staged_chip_the_badge_and_the_cards() {
        crate::pages::approvals_cache(crate::pages::ApprovalsView {
            cards: vec![crate::pages::ApprovalCard {
                id: "a-1".into(),
                headline: "Reach example.com".into(),
                summary: "Fetch a page".into(),
                details: vec!["Host: example.com".into()],
                choices: vec![crate::pages::ApprovalChoice {
                    word: "deny".into(),
                    label: "Deny".into(),
                }],
            }],
            loaded: true,
            note: None,
        });
        crate::pages::chat_cache(HeldChat {
            unread: 3,
            staged: vec![crate::pages::StagedChip {
                name: "pasted-1.png".into(),
                width: 10,
                height: 20,
                bytes: 33,
            }],
            ..HeldChat::default()
        });
        let (_, answer, _) = handle_command_with(r#"{"id":81,"cmd":"chat.snapshot"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a cache read answers at once")
        };
        assert_eq!(reply["unread"], serde_json::json!(3));
        assert_eq!(reply["staged"][0]["name"], "pasted-1.png");
        assert_eq!(reply["staged"][0]["width"], serde_json::json!(10));
        assert_eq!(reply["staged"][0]["bytes"], serde_json::json!(33));
        assert_eq!(reply["approvals"]["loaded"], serde_json::json!(true));
        assert_eq!(
            reply["approvals"]["cards"][0]["headline"],
            "Reach example.com"
        );
        assert_eq!(reply["approvals"]["cards"][0]["choices"][0]["word"], "deny");
        crate::pages::approvals_cache(crate::pages::ApprovalsView::default());
    }

    /// A paste, a drop, a re-read and a decision each reach their own route
    /// once, and a window without the routes can do none of them -- there is
    /// no second path into the composer, the clipboard or the service.
    #[test]
    fn chat_attach_unstage_approvals_and_decide_take_their_own_routes() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let (attach_seen, unstage_seen) = (seen.clone(), seen.clone());
        let (approvals_seen, decide_seen) = (seen.clone(), seen.clone());
        let routes = WebRoutes {
            attach: Rc::new(move || {
                attach_seen.borrow_mut().push("attach".to_owned());
                Ok(())
            }),
            unstage: Rc::new(move |name: &str| {
                unstage_seen.borrow_mut().push(format!("unstage {name}"));
                Ok(())
            }),
            approvals: Rc::new(move || {
                approvals_seen.borrow_mut().push("approvals".to_owned());
                Ok(())
            }),
            decide: Rc::new(move |id: &str, word: &str| {
                decide_seen.borrow_mut().push(format!("decide {id} {word}"));
                Ok(())
            }),
            ..stub_routes()
        };
        for request in [
            r#"{"id":91,"cmd":"chat.attach"}"#,
            r#"{"id":92,"cmd":"chat.unstage","name":"pasted-1.png"}"#,
            r#"{"id":93,"cmd":"chat.approvals.refresh"}"#,
            r#"{"id":94,"cmd":"chat.decide","approval_id":"a-1","decision":"allow_once"}"#,
        ] {
            let (_, answer, _) = handle_command_with(request, None, Some(&routes));
            let Answer::Now(reply) = answer else {
                panic!("a page control answers at once")
            };
            assert_eq!(reply["ok"], serde_json::json!(true), "{request}");
        }
        assert_eq!(
            seen.borrow().as_slice(),
            [
                "attach",
                "unstage pasted-1.png",
                "approvals",
                "decide a-1 allow_once"
            ]
        );

        // A press the app could not take comes back as its own sentence, not
        // as an acceptance: a held panel slot is a dropped press, and the page
        // must show that instead of a card that looks decided.
        let busy = WebRoutes {
            decide: Rc::new(|_: &str, _: &str| {
                Err("The approvals panel is still finishing the last press; \
                     try again in a moment."
                    .to_owned())
            }),
            ..stub_routes()
        };
        let (id, answer, _) = handle_command_with(
            r#"{"id":99,"cmd":"chat.decide","approval_id":"a-1","decision":"deny"}"#,
            None,
            Some(&busy),
        );
        // The envelope's id is the reply ticket: the approval's own id must
        // ride beside it, or the page never hears back.
        assert_eq!(id, serde_json::json!(99));
        let Answer::Now(reply) = answer else {
            panic!("a refused decision answers at once")
        };
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert!(
            reply["error"]
                .as_str()
                .unwrap_or("")
                .contains("still finishing")
        );

        // A window opened without the routes cannot reach any of them.
        for (request, refused) in [
            (
                r#"{"id":95,"cmd":"chat.attach"}"#,
                "this window cannot attach a picture",
            ),
            (
                r#"{"id":96,"cmd":"chat.unstage","name":"x"}"#,
                "this window cannot drop a picture",
            ),
            (
                r#"{"id":97,"cmd":"chat.approvals.refresh"}"#,
                "this window cannot read approvals",
            ),
            (
                r#"{"id":98,"cmd":"chat.decide","approval_id":"a-1","decision":"deny"}"#,
                "this window cannot decide approvals",
            ),
        ] {
            let (_, answer, _) = handle_command_with(request, None, None);
            let Answer::Now(reply) = answer else {
                panic!("a closed route answers at once")
            };
            assert_eq!(reply["ok"], serde_json::json!(false));
            assert_eq!(reply["error"], serde_json::json!(refused));
        }
    }

    /// Threads boot: one page-tagged payload with the native page's grouping,
    /// pin order and side-chat split.
    #[test]
    fn the_threads_document_names_its_page_and_carries_the_rows() {
        let held = HeldThreads {
            threads: vec![
                crate::pages::ThreadRow {
                    id: "s-1".into(),
                    title: "A calmer inbox".into(),
                    meta: "digest only · 09:14".into(),
                    group: "Today".into(),
                    unread: 2,
                    pinned: false,
                    side: false,
                },
                crate::pages::ThreadRow {
                    id: "s-2".into(),
                    title: "Side question".into(),
                    pinned: true,
                    side: true,
                    ..crate::pages::ThreadRow::default()
                },
            ],
            total: 2,
            loaded: true,
            error: None,
        };
        let doc = threads_document(&held);
        assert!(doc.contains("\"page\":\"threads\""), "page named in boot");
        assert!(doc.contains("A calmer inbox"));
        assert!(doc.contains("digest only"));
        assert!(doc.contains("\"unread\":2"));
        assert!(doc.contains("\"side\":true"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    #[test]
    fn threads_snapshot_answers_from_the_held_rows() {
        crate::pages::threads_cache(HeldThreads {
            threads: vec![crate::pages::ThreadRow {
                id: "s-7".into(),
                title: "Held thread".into(),
                group: "Yesterday".into(),
                ..crate::pages::ThreadRow::default()
            }],
            total: 1,
            loaded: true,
            error: None,
        });
        let (id, answer, command) =
            handle_command_with(r#"{"id":41,"cmd":"threads.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(41));
        assert_eq!(command, "threads.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["threads"][0]["title"], "Held thread");
        assert_eq!(reply["threads"][0]["group"], "Yesterday");
        assert_eq!(reply["total"], 1);
        assert_eq!(reply["loaded"], true);
    }

    /// Threads refresh reads the account listing on a thread: the same
    /// conversation read the native connect makes.
    #[test]
    fn threads_refresh_reads_the_service() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-threads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        // The rows a refresh answers with are the listing plus any archive
        // memory; under test there is none, so the answer is the listing alone.
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let service = serde_json::json!({
            "ok": true,
            "data": {
                "identity_name": "Muse",
                "vm_id": "vm-1",
                "threads": [
                    {
                        "session_id": "s-1", "title": "Read from the service",
                        "archived": false, "pinned": true, "unread_count": 4,
                        "snippet": "latest activity", "updated_at": "2026-11-20T09:14:00Z"
                    },
                    {
                        "session_id": "s-2", "title": "A side chat",
                        "archived": false, "is_primary": false,
                        "updated_at": "2026-11-19T18:00:00Z"
                    }
                ]
            }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("request");
            let mut writer = stream;
            writer.write_all(service.as_bytes()).expect("reply");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
            line
        });

        crate::pages::threads_cache(HeldThreads {
            threads: vec![crate::pages::ThreadRow {
                id: "held".into(),
                title: "Held, not re-read".into(),
                ..crate::pages::ThreadRow::default()
            }],
            total: 1,
            loaded: true,
            error: None,
        });
        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);

        let (_, answer, command) =
            handle_command_with(r#"{"id":42,"cmd":"threads.refresh"}"#, Some(client), None);
        assert_eq!(command, "threads.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");
        let Fetched::Threads(rows) = fetched else {
            panic!("the host answered with something else");
        };
        // Pinned first, the side chat kept apart, the meta line joined.
        assert_eq!(rows.threads[0].title, "Read from the service");
        assert!(rows.threads[0].pinned);
        assert_eq!(rows.threads[0].meta, "latest activity · 09:14");
        assert_eq!(rows.threads[0].unread, 4);
        assert_eq!(rows.threads[1].title, "A side chat");
        assert!(rows.threads[1].side);
        assert_eq!(rows.total, 2);

        let reply = reply_for(Fetched::Threads(rows), &command, Instant::now());
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["threads"][0]["title"], "Read from the service");
        let cached = crate::pages::threads_snapshot();
        assert_eq!(cached.threads[0].title, "Read from the service");
        assert_eq!(cached.total, 2);

        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"connect""#), "{request}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn threads_refresh_without_a_client_reports_the_missing_link() {
        let (_, answer, command) =
            handle_command_with(r#"{"id":43,"cmd":"threads.refresh"}"#, None, None);
        assert_eq!(command, "threads.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no rows"),
        }
    }

    /// One held conversation row, so a route test speaks about a real id.
    fn hold_one_thread() {
        crate::pages::threads_cache(HeldThreads {
            threads: vec![crate::pages::ThreadRow {
                id: "s-1".into(),
                title: "A calmer inbox".into(),
                ..crate::pages::ThreadRow::default()
            }],
            total: 1,
            loaded: true,
            error: None,
        });
    }

    #[test]
    fn thread_ops_refuse_rows_the_page_invented_and_verbs_it_cannot_send() {
        hold_one_thread();

        let (_, answer, command) = handle_command_with(
            r#"{"id":51,"cmd":"threads.op","thread":"made-up","verb":"pin"}"#,
            None,
            None,
        );
        assert_eq!(command, "threads.op");
        let Answer::Now(reply) = answer else {
            panic!("a refusal answers now")
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(
            reply["error"],
            "that conversation is not in the held listing"
        );

        let (_, answer, _) = handle_command_with(
            r#"{"id":52,"cmd":"threads.op","thread":"s-1","verb":"rename","title":"   "}"#,
            None,
            None,
        );
        let Answer::Now(reply) = answer else {
            panic!("a blank title answers now")
        };
        assert_eq!(reply["error"], "a thread needs a non-blank title");

        let (_, answer, _) = handle_command_with(
            r#"{"id":53,"cmd":"threads.op","thread":"s-1","verb":"explode"}"#,
            None,
            None,
        );
        let Answer::Now(reply) = answer else {
            panic!("an unknown verb answers now")
        };
        assert_eq!(
            reply["error"],
            "that is not a conversation change this app makes"
        );

        let (_, answer, _) = handle_command_with(
            r#"{"id":54,"cmd":"threads.op","thread":"s-1","verb":"rename"}"#,
            None,
            None,
        );
        let Answer::Now(reply) = answer else {
            panic!("a rename without a title answers now")
        };
        assert_eq!(reply["error"], "a rename needs a title");
    }

    /// Deleting a conversation is the one verb that writes nothing on its
    /// first press: the row is armed, and only the second press reaches the
    /// service (which, with no link here, is where it stops).
    #[test]
    fn deleting_a_conversation_arms_first_and_writes_on_the_second_press() {
        hold_one_thread();

        let (_, answer, _) = handle_command_with(
            r#"{"id":56,"cmd":"threads.op","thread":"s-1","verb":"delete"}"#,
            None,
            None,
        );
        let Answer::Now(reply) = answer else {
            panic!("the arming press answers at once, with nothing written")
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["accepted"], false);
        assert_eq!(reply["armed"], "s-1");
        assert_eq!(reply["threads"][0]["id"], "s-1");
        assert_eq!(crate::pages::thread_delete_armed().as_deref(), Some("s-1"));

        let (_, answer, _) = handle_command_with(
            r#"{"id":57,"cmd":"threads.op","thread":"s-1","verb":"delete","title":"ignored"}"#,
            None,
            None,
        );
        let Answer::Later(receiver) = answer else {
            panic!("the confirming press leaves the main loop")
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no delete"),
        }
        assert_eq!(
            crate::pages::thread_delete_armed(),
            None,
            "the arm is spent"
        );
    }

    /// A painting of the listing clears the arm: a press aimed at a row the
    /// service has since moved cannot delete what took its place.
    #[test]
    fn a_repainted_listing_spends_the_delete_arm() {
        hold_one_thread();
        let (_, answer, _) = handle_command_with(
            r#"{"id":58,"cmd":"threads.op","thread":"s-1","verb":"delete"}"#,
            None,
            None,
        );
        assert!(matches!(answer, Answer::Now(_)));
        assert_eq!(crate::pages::thread_delete_armed().as_deref(), Some("s-1"));
        hold_one_thread();
        assert_eq!(crate::pages::thread_delete_armed(), None);
        // And a second delete press after the repaint arms again instead of
        // deleting something nobody confirmed.
        let (_, answer, _) = handle_command_with(
            r#"{"id":59,"cmd":"threads.op","thread":"s-1","verb":"delete"}"#,
            None,
            None,
        );
        assert!(matches!(answer, Answer::Now(_)));
    }

    /// Clearing a badge is the same discipline as a rename: a row the held
    /// listing does not carry is refused here, and with no link the write
    /// stops at the worker instead of pretending a change.
    #[test]
    fn marking_a_conversation_read_refuses_rows_it_does_not_hold() {
        hold_one_thread();
        crate::pages::threads_cache(HeldThreads {
            threads: vec![crate::pages::ThreadRow {
                id: "s-1".into(),
                title: "A calmer inbox".into(),
                unread: 2,
                ..crate::pages::ThreadRow::default()
            }],
            total: 1,
            loaded: true,
            error: None,
        });

        let (_, answer, command) = handle_command_with(
            r#"{"id":61,"cmd":"threads.seen","thread":"made-up"}"#,
            None,
            None,
        );
        assert_eq!(command, "threads.seen");
        let Answer::Now(reply) = answer else {
            panic!("a refusal answers now")
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(
            reply["error"],
            "that conversation is not in the held listing"
        );

        let (_, answer, _) = handle_command_with(
            r#"{"id":62,"cmd":"threads.seen","thread":"s-1"}"#,
            None,
            None,
        );
        let Answer::Later(receiver) = answer else {
            panic!("a write leaves the main loop")
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no read mark"),
        }
    }

    #[test]
    fn a_thread_op_with_no_client_reports_the_missing_link() {
        hold_one_thread();
        let (_, answer, _) = handle_command_with(
            r#"{"id":55,"cmd":"threads.op","thread":"s-1","verb":"pin"}"#,
            None,
            None,
        );
        let Answer::Later(receiver) = answer else {
            panic!("a change leaves the main loop")
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no change"),
        }
    }

    #[test]
    fn a_thread_op_reply_carries_the_service_word_and_the_held_rows() {
        let held = HeldThreads {
            threads: vec![crate::pages::ThreadRow {
                id: "s-1".into(),
                title: "Renamed by the service".into(),
                ..crate::pages::ThreadRow::default()
            }],
            total: 1,
            loaded: true,
            error: None,
        };
        crate::pages::threads_cache(held.clone());
        let reply = reply_for(
            Fetched::ThreadOp {
                id: "s-1".into(),
                verb: "rename".into(),
                accepted: true,
                error: None,
                rows: Some(held),
                threads: Vec::new(),
                reread_error: None,
            },
            "threads.op",
            Instant::now(),
        );
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["accepted"], true);
        assert_eq!(reply["thread"], "s-1");
        assert_eq!(reply["threads"][0]["title"], "Renamed by the service");
        assert_eq!(reply["total"], 1);
        assert_eq!(reply["loaded"], true);
    }

    /// The row projection is the native wording: pinned first, day groups
    /// from the stamp against the caller's today, side chats apart, and an
    /// untitled conversation still gets a row.
    #[test]
    fn thread_rows_group_the_way_the_native_list_does() {
        use muse_account::Thread;

        fn thread(id: &str, title: &str, stamp: Option<&str>) -> Thread {
            Thread {
                session_id: id.into(),
                title: title.into(),
                archived: false,
                pinned: false,
                is_thread: true,
                unread_count: 0,
                snippet: None,
                is_primary: true,
                updated_at: stamp.map(str::to_owned),
            }
        }

        let today = "2026-11-20";
        let mut pinned = thread("s-0", "Pinned", Some("2026-11-18T08:00:00Z"));
        pinned.pinned = true;
        let mut side = thread("s-3", "Side", Some("2026-11-20T10:00:00Z"));
        side.is_primary = false;
        let rows = crate::pages::held_threads_from(
            &[
                thread("s-1", "Today thread", Some("2026-11-20T09:14:00Z")),
                pinned,
                thread("s-2", "Older thread", Some("2026-11-17T09:00:00Z")),
                side,
                thread("s-4", "", None),
            ],
            today,
        );

        assert_eq!(rows.threads[0].title, "Pinned", "pinned rows lead");
        assert!(rows.threads[0].pinned);
        assert_eq!(rows.threads[1].group, "Today");
        assert_eq!(rows.threads[1].meta, "09:14");
        assert_eq!(rows.threads[2].group, "2026-11-17");
        // The side chat keeps its listing position; only the pin order is
        // imposed, and it is stable.
        assert_eq!(rows.threads[3].title, "Side");
        assert!(rows.threads[3].side);
        assert_eq!(rows.threads[4].title, "(untitled)");
        assert_eq!(rows.threads[4].group, "Earlier");
        assert_eq!(rows.total, 5);
        assert!(rows.loaded);
    }

    /// Feed boot: one page-tagged payload carrying the prompt card, the
    /// generation line and the cards in the native wording.
    #[test]
    fn the_feed_document_names_its_page_and_carries_prompt_and_cards() {
        use muse_account::{FeedPrompt, FeedStatus, ListingItem, ServiceListing};

        let listing = ServiceListing {
            total: 2,
            recognized: 2,
            skipped: 0,
            items: vec![
                ListingItem {
                    id: Some("u-1".into()),
                    title: "A unit title".into(),
                    icon: Some("🧭".into()),
                    body: Some("Read [the advisory](https://example.test/a) now.".into()),
                    kicker: Some("Research".into()),
                    image_url: Some("https://example.test/hero.jpg".into()),
                    widget_html: Some("<b>graphic</b>".into()),
                    widget_text: Some("graphic text".into()),
                    action_label: Some("Open".into()),
                    why: Some("Because you asked".into()),
                    stamp: Some("2026-11-20T09:00:00Z".into()),
                    ..ListingItem::default()
                },
                ListingItem {
                    id: Some("u-2".into()),
                    title: "Second unit".into(),
                    ..ListingItem::default()
                },
            ],
            feed_prompt: Some(FeedPrompt {
                text: Some("Verdant architecture".into()),
                enabled: true,
            }),
            feed_status: Some(FeedStatus {
                refresh_in_progress: true,
                next_edition_kind: None,
            }),
        };
        let held = crate::pages::held_feed_from(&listing, 1_795_000_000, true);
        // The first graphical card renders live, the second has no markup.
        assert!(held.cards[0].live);
        assert_eq!(held.cards[0].meta, "Research");
        // The body travels as the native markup, links and all.
        assert_eq!(
            held.cards[0].body,
            "Read <a href=\"https://example.test/a\">the advisory</a> now."
        );
        assert_eq!(held.cards[0].action, "Open");
        assert_eq!(held.cards[0].why, "Because you asked");
        assert_eq!(held.cards[0].icon, "🧭");
        assert_eq!(held.cards[0].widget_text, "graphic text");
        assert_eq!(held.cards[1].icon, "📰", "the native default icon");
        assert!(!held.cards[1].live);

        let doc = feed_document(&held);
        assert!(doc.contains("\"page\":\"feed\""), "page named in boot");
        assert!(doc.contains("Verdant architecture"));
        assert!(doc.contains("Generating a fresh edition"));
        assert!(doc.contains("A unit title"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    #[test]
    fn feed_snapshot_answers_from_the_held_cards() {
        crate::pages::feed_cache(HeldFeed {
            cards: vec![crate::pages::FeedCard {
                id: "u-9".into(),
                title: "Held card".into(),
                ..crate::pages::FeedCard::default()
            }],
            prompt: "Held prompt".into(),
            total: 1,
            loaded: true,
            ..HeldFeed::default()
        });
        let (id, answer, command) =
            handle_command_with(r#"{"id":51,"cmd":"feed.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(51));
        assert_eq!(command, "feed.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["cards"][0]["title"], "Held card");
        assert_eq!(reply["prompt"], "Held prompt");
        assert_eq!(reply["loaded"], true);
    }

    /// Feed refresh reads the service on a thread and updates the cache.
    #[test]
    fn feed_refresh_reads_the_service() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-feed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let service = serde_json::json!({
            "ok": true,
            "data": {
                "total": 3, "recognized": 1, "skipped": 0,
                "items": [{ "id": "u-1", "title": "Read from the service" }],
                "feed_prompt": { "text": "Fresh prompt", "enabled": true },
                "feed_status": { "refresh_in_progress": false }
            }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("request");
            let mut writer = stream;
            writer.write_all(service.as_bytes()).expect("reply");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
            line
        });

        crate::pages::feed_cache(HeldFeed {
            cards: vec![crate::pages::FeedCard {
                id: "held".into(),
                title: "Held, not re-read".into(),
                ..crate::pages::FeedCard::default()
            }],
            total: 1,
            loaded: true,
            ..HeldFeed::default()
        });
        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);

        let (_, answer, command) =
            handle_command_with(r#"{"id":52,"cmd":"feed.refresh"}"#, Some(client), None);
        assert_eq!(command, "feed.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");
        let Fetched::Feed(rows) = fetched else {
            panic!("the host answered with something else");
        };
        assert_eq!(rows.cards[0].title, "Read from the service");
        assert_eq!(rows.prompt, "Fresh prompt");
        assert_eq!(rows.total, 3);

        let reply = reply_for(Fetched::Feed(rows), &command, Instant::now());
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["cards"][0]["title"], "Read from the service");
        let cached = crate::pages::feed_snapshot();
        assert_eq!(cached.cards[0].title, "Read from the service");
        assert_eq!(cached.prompt, "Fresh prompt");

        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"feed""#), "{request}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn feed_refresh_without_a_client_reports_the_missing_link() {
        let (_, answer, command) =
            handle_command_with(r#"{"id":53,"cmd":"feed.refresh"}"#, None, None);
        assert_eq!(command, "feed.refresh");
        let Answer::Later(receiver) = answer else {
            panic!("refresh must leave the main loop");
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no rows"),
        }
    }

    /// The hero command answers a data URI from the cache, and says so
    /// plainly when nothing is cached -- never a `file://` path the page
    /// could not read anyway.
    /// Search boots with the query that produced the rows, so the page can
    /// show what was searched. Rows without an id are dropped, as the native
    /// paint drops them (a result with no conversation cannot be opened).
    #[test]
    fn the_search_document_names_its_page_and_carries_query_and_rows() {
        use muse_account::{ListingItem, ServiceListing};

        let listing = ServiceListing {
            total: 3,
            recognized: 2,
            skipped: 1,
            items: vec![
                ListingItem {
                    id: Some("thread-9".into()),
                    title: "Visitor parking".into(),
                    icon: Some("🚗".into()),
                    kicker: Some("Rowyn".into()),
                    kind: Some("message".into()),
                    stamp: Some("2026-11-20T09:14:00Z".into()),
                    ..ListingItem::default()
                },
                ListingItem {
                    id: None,
                    title: "No conversation, no row".into(),
                    ..ListingItem::default()
                },
            ],
            feed_prompt: None,
            feed_status: None,
        };
        let held = crate::pages::held_search_from("parking", &listing);
        assert_eq!(held.rows.len(), 1, "the id-less item is dropped");
        assert_eq!(held.rows[0].id, "thread-9");
        assert_eq!(held.rows[0].icon, "🚗");
        assert_eq!(held.rows[0].meta, "Rowyn · message · 2026-11-20T09:14:00Z");
        assert_eq!(held.total, 3, "the service's total, not the row count");
        assert!(held.loaded);

        let doc = search_document(&held);
        assert!(doc.contains("\"page\":\"search\""), "page named in boot");
        assert!(doc.contains("parking"));
        assert!(doc.contains("Visitor parking"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    #[test]
    fn search_snapshot_answers_from_the_held_rows() {
        crate::pages::search_cache(HeldSearch {
            query: "held query".into(),
            rows: vec![crate::pages::SearchRow {
                id: "thread-1".into(),
                title: "Held result".into(),
                ..crate::pages::SearchRow::default()
            }],
            total: 1,
            loaded: true,
            error: None,
        });
        let (id, answer, command) =
            handle_command_with(r#"{"id":61,"cmd":"search.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(61));
        assert_eq!(command, "search.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["query"], "held query");
        assert_eq!(reply["rows"][0]["title"], "Held result");
    }

    /// The folder sheet's one read: the subfolders of the path it names, the
    /// parent it can step to, and nothing that is not a folder. A refusal is a
    /// sentence with `ok: false`, never an empty list the sheet could paint as
    /// "this folder has none".
    #[test]
    fn the_folder_sheet_lists_folders_and_refuses_what_it_cannot_read() {
        let dir = std::env::temp_dir().join(format!("muse-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Beta")).expect("beta folder");
        std::fs::create_dir_all(dir.join("alpha")).expect("alpha folder");
        std::fs::write(dir.join("notes.txt"), b"not a folder").expect("a file beside them");

        let listing = folder_listing(&dir.display().to_string());
        assert_eq!(listing["ok"], true, "the folder reads: {listing}");
        assert_eq!(listing["path"], dir.display().to_string());
        assert_eq!(
            listing["parent"],
            dir.parent().expect("a parent").display().to_string()
        );
        let names: Vec<&str> = listing["folders"]
            .as_array()
            .expect("rows")
            .iter()
            .filter_map(|row| row["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec!["alpha", "Beta"],
            "folders only, sorted: {names:?}"
        );
        assert_eq!(
            listing["folders"][1]["path"],
            dir.join("Beta").display().to_string(),
            "a row carries the path the sheet returns"
        );

        let relative = folder_listing("relative/folder");
        assert_eq!(relative["ok"], false);
        assert_eq!(relative["error"], "The folder path must be absolute.");
        let missing = folder_listing("/definitely/not/a/folder");
        assert_eq!(missing["ok"], false);
        assert!(
            missing["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("Cannot read that folder:")),
            "a missing path is the app's sentence: {missing}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sheet's create press: one child of the folder the listing just read,
    /// and every refusal in the app's own words.
    #[test]
    fn the_sheet_creates_one_folder_and_refuses_the_rest() {
        let dir = std::env::temp_dir().join(format!("muse-folder-create-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a folder to create in");
        let parent = dir.display().to_string();

        let made = folder_create(&parent, "  parity-session  ");
        assert_eq!(made["ok"], true, "the folder is made: {made}");
        assert_eq!(
            made["path"],
            dir.join("parity-session").display().to_string(),
            "the sheet steps into the path made"
        );
        assert!(dir.join("parity-session").is_dir(), "the child is on disk");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join("parity-session"))
                .expect("the folder's own mode")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "a session folder is private: {mode:o}");
        }

        let again = folder_create(&parent, "parity-session");
        assert_eq!(again["ok"], false);
        assert_eq!(again["error"], "parity-session is already in this folder.");

        for (name, sentence) in [
            ("", "Type a name for the new folder."),
            (".", "That is not a folder name."),
            ("..", "That is not a folder name."),
            ("a/b", "A folder name cannot contain a slash."),
            ("a\\b", "A folder name cannot contain a slash."),
            (
                "bad\u{7}name",
                "A folder name cannot contain control characters.",
            ),
        ] {
            let refused = folder_create(&parent, name);
            assert_eq!(refused["ok"], false, "{name:?} is refused");
            assert_eq!(refused["error"], sentence, "{name:?}");
        }

        let relative = folder_create("relative/folder", "child");
        assert_eq!(relative["ok"], false);
        assert_eq!(relative["error"], "The folder path must be absolute.");
        // A missing parent is made with the folder: a first launch's own
        // session folder does not exist yet, and the sheet opens on it.
        let missing = folder_create(&dir.join("not-yet").display().to_string(), "child");
        assert_eq!(missing["ok"], true, "the chain is made: {missing}");
        assert!(dir.join("not-yet").join("child").is_dir());

        // A parent that is a file is refused, in the app's words.
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"a file, not a folder").expect("a file to refuse");
        let not_a_folder = folder_create(&file.display().to_string(), "child");
        assert_eq!(not_a_folder["ok"], false);
        assert_eq!(not_a_folder["error"], "That path is a file, not a folder.");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Settings opens on the app's own held state: the updater's last answer
    /// and the policy the chat panel read, with the page named for the bundle.
    #[test]
    fn the_settings_document_names_its_page_and_carries_the_held_state() {
        crate::pages::settings_updates_cache(crate::pages::UpdatesView {
            appimage: true,
            status: "Up to date (build 62ce6e3).".into(),
            busy: false,
            staged: false,
            check_label: crate::pages::UPDATE_CHECK_LABEL.into(),
            action_label: None,
        });
        crate::pages::settings_policy_cache(crate::pages::PolicyView {
            loaded: true,
            rows: vec![crate::pages::PolicyRowView {
                label: "connector_default".into(),
                value: "always_ask".into(),
                flip: Some("auto_allow".into()),
            }],
            connectors: vec!["calendar".into()],
            withheld: 2,
            note: None,
        });

        let held = crate::pages::settings_snapshot();
        let doc = settings_document(&held);
        assert!(doc.contains("\"page\":\"settings\""), "page named in boot");
        assert!(doc.contains("Up to date (build 62ce6e3)."));
        assert!(doc.contains("connector_default"));
        assert!(doc.contains("auto_allow"));
        assert!(doc.contains("calendar"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    /// The channel answers the page's one read from the same held state, so a
    /// reload never becomes a wire trip.
    #[test]
    fn settings_snapshot_answers_from_the_held_state() {
        crate::pages::settings_updates_cache(crate::pages::UpdatesView {
            appimage: false,
            status: "Up to date.".into(),
            busy: false,
            staged: true,
            check_label: crate::pages::UPDATE_CHECK_LABEL.into(),
            action_label: Some(crate::pages::UPDATE_RESTART_LABEL.into()),
        });
        crate::pages::settings_policy_cache(crate::pages::PolicyView {
            loaded: true,
            rows: vec![crate::pages::PolicyRowView {
                label: "network_default".into(),
                value: "auto_allow".into(),
                flip: None,
            }],
            connectors: Vec::new(),
            withheld: 0,
            note: None,
        });

        let (id, answer, command) =
            handle_command_with(r#"{"id":71,"cmd":"settings.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(71));
        assert_eq!(command, "settings.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a held-state read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["updates"]["status"], "Up to date.");
        assert_eq!(reply["updates"]["staged"], true);
        assert_eq!(reply["policy"]["rows"][0]["label"], "network_default");
        assert!(reply["policy"]["rows"][0]["flip"].is_null());
    }

    /// Sign-out travels as the app's own press: the page gets the app's answer
    /// on its ticket, and a window without routes cannot clear the link at all.
    #[test]
    fn a_sign_out_press_reaches_the_apps_own_route() {
        let mut routes = signin_surface(crate::web_signin::routes(|| {}));
        routes.sign_out = std::rc::Rc::new(|| Err("the app refused".to_owned()));

        let (_, answer, command) =
            handle_command_with(r#"{"id":9,"cmd":"settings.sign_out"}"#, None, Some(&routes));
        assert_eq!(command, "settings.sign_out");
        let Answer::Now(reply) = answer else {
            panic!("a press answers at once")
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "the app refused");

        let (_, answer, _) =
            handle_command_with(r#"{"id":10,"cmd":"settings.sign_out"}"#, None, None);
        let Answer::Now(reply) = answer else {
            panic!("a refusal answers at once")
        };
        assert_eq!(reply["error"], "this window cannot make that change");

        // The sign-in surface has no link of its own to clear, and says so
        // rather than accepting a press it cannot carry out.
        let (_, answer, _) = handle_command_with(
            r#"{"id":11,"cmd":"settings.sign_out"}"#,
            None,
            Some(&signin_surface(crate::web_signin::routes(|| {}))),
        );
        let Answer::Now(reply) = answer else {
            panic!("a refusal answers at once")
        };
        assert_eq!(
            reply["error"],
            "This window is the sign-in surface; it cannot sign out."
        );
    }

    /// The Local host page opens on the host's read model, named for the
    /// bundle, so a fresh window paints the same grants, cards and log the
    /// native section already shows.
    #[test]
    fn the_localhost_document_names_its_page_and_carries_the_held_state() {
        crate::pages::localhost_cache(crate::pages::HeldLocalHost {
            identity: "This machine registers as its own device.".into(),
            search_head: crate::pages::HOST_SEARCH_HEAD.into(),
            search: None,
            searching: false,
            search_summary: "No search yet.".into(),
            phase: "registered".into(),
            notice: "Listening for the account's own node commands.".into(),
            device: Some("linux-0123456789abcdef".into()),
            grants: vec![crate::pages::LocalHostGrantRow {
                label: "Scratch".into(),
                root: "/var/home/phendrick/Scratch".into(),
                writable: true,
                policy: false,
                text: "Scratch · /var/home/phendrick/Scratch · read-write".into(),
            }],
            pending: vec![crate::pages::LocalHostCardRow {
                id: "a-1".into(),
                operation: "create_text".into(),
                path: "Scratch/notes.txt".into(),
                target: None,
                preview: Some("hello from the agent".into()),
                deciding: false,
                countdown: "Expires in 42s".into(),
            }],
            log: vec!["files.read_text Scratch/notes.txt → allowed".into()],
            ..Default::default()
        });

        let held = crate::pages::localhost_snapshot();
        let doc = localhost_document(&held);
        assert!(doc.contains("\"page\":\"localhost\""), "page named in boot");
        assert!(doc.contains("Listening for the account's own node commands."));
        assert!(doc.contains("create_text"));
        assert!(doc.contains("hello from the agent"));
        assert!(!doc.contains("/*__BOOT__*/"));
    }

    /// A press made through a route must change what the page reads next. The
    /// held cache is written by the native paint, and a window whose Local host
    /// page is the only surface open has no paint of its own -- so a press that
    /// only mutated the controller left the page showing the cards, log and
    /// switch from before it.
    #[test]
    fn a_localhost_press_re_projects_the_snapshot_the_page_reads() {
        use std::rc::Rc;
        // What the last paint left in the cache.
        crate::pages::localhost_cache(crate::pages::HeldLocalHost {
            notice: "the paint's own notice".into(),
            log: vec!["node registered".into()],
            ..Default::default()
        });
        let controller = Rc::new(std::cell::RefCell::new(crate::local_host::Controller::new()));
        let press = {
            let controller = controller.clone();
            Rc::new(move || {
                crate::pages::press_localhost(&mut controller.borrow_mut(), |host| {
                    host.clear_log()
                });
                Ok(())
            })
        };
        let routes = WebRoutes {
            host_clear_log: press,
            ..stub_routes()
        };

        let (_, before, _) =
            handle_command_with(r#"{"id":90,"cmd":"localhost.snapshot"}"#, None, None);
        let Answer::Now(before) = before else {
            panic!("a held-state read answers at once")
        };
        assert_eq!(before["log"], serde_json::json!(["node registered"]));

        let (_, answer, _) = handle_command_with(
            r#"{"id":91,"cmd":"localhost.clear_log"}"#,
            None,
            Some(&routes),
        );
        let Answer::Now(answer) = answer else {
            panic!("a route press answers at once")
        };
        assert_eq!(answer["ok"], serde_json::json!(true));

        let (_, after, _) =
            handle_command_with(r#"{"id":92,"cmd":"localhost.snapshot"}"#, None, None);
        let Answer::Now(after) = after else {
            panic!("a held-state read answers at once")
        };
        assert_eq!(after["log"], serde_json::json!([]));
        // The page now reads the controller's projection, not the old paint's.
        assert_eq!(
            after["notice"],
            serde_json::json!("Disconnected. No folders shared with Muse.")
        );
    }

    /// The page's snapshot is also its pump: when no native page is open there
    /// is nothing else polling the controller, so a queued grant or search
    /// would never land. Every read polls first, then answers the projection.
    #[test]
    fn the_page_snapshot_polls_before_it_answers() {
        use std::rc::Rc;
        crate::pages::localhost_cache(crate::pages::HeldLocalHost {
            notice: "before the poll".into(),
            ..Default::default()
        });
        let refresh = Rc::new(|| {
            crate::pages::localhost_cache(crate::pages::HeldLocalHost {
                notice: "after the poll".into(),
                search: Some(crate::local_host::FileSearch {
                    query: "report".into(),
                    hits: Vec::new(),
                    refused: Vec::new(),
                    scanned: 3,
                    truncated: false,
                }),
                ..Default::default()
            });
            Ok(())
        });
        let routes = WebRoutes {
            host_refresh: refresh,
            ..stub_routes()
        };

        let (_, answer, _) = handle_command_with(
            r#"{"id":80,"cmd":"localhost.snapshot"}"#,
            None,
            Some(&routes),
        );
        let Answer::Now(answer) = answer else {
            panic!("a held-state read answers at once")
        };
        assert_eq!(answer["notice"], serde_json::json!("after the poll"));
        assert_eq!(answer["search"]["query"], serde_json::json!("report"));
        assert_eq!(answer["search"]["scanned"], serde_json::json!(3));
    }

    /// The page's filename search and preview are the app's own: the query is
    /// passed through untouched, an answer carries the app's text and the path
    /// it answered for, a refusal is a refusal (never an empty preview posing
    /// as a file), and a window that cannot read says so.
    #[test]
    fn the_page_search_routes_answer_with_the_apps_own_words() {
        use std::rc::Rc;
        let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
        let asked = seen.clone();
        let search = Rc::new(move |query: &str| {
            asked.borrow_mut().push(query.to_owned());
            Ok(())
        });
        let preview = Rc::new(|folder: &str, path: &str| {
            if path == "missing.txt" {
                return Err("That folder is no longer granted. Search again.".to_owned());
            }
            Ok(serde_json::json!({
                "folder": folder,
                "path": path,
                "content": "the report\n",
            }))
        });
        let content_asked = seen.clone();
        let content = Rc::new(move |query: &str| {
            content_asked.borrow_mut().push(query.to_owned());
            Ok(())
        });
        let routes = WebRoutes {
            host_search: search,
            host_content: content,
            host_preview: preview,
            ..stub_routes()
        };

        let (_, answer, _) = handle_command_with(
            r#"{"id":70,"cmd":"localhost.search","query":"report"}"#,
            None,
            Some(&routes),
        );
        let Answer::Now(answer) = answer else {
            panic!("a search press answers at once")
        };
        assert_eq!(answer["ok"], serde_json::json!(true));

        // The page's content search takes the same route: the app validates
        // the text and queues the broker's own read.
        let (_, content, _) = handle_command_with(
            r#"{"id":74,"cmd":"localhost.content","query":"revenue"}"#,
            None,
            Some(&routes),
        );
        let Answer::Now(content) = content else {
            panic!("a content press answers at once")
        };
        assert_eq!(content["ok"], serde_json::json!(true));
        assert_eq!(
            seen.borrow().as_slice(),
            ["report".to_owned(), "revenue".to_owned()]
        );

        let (_, held, _) = handle_command_with(
            r#"{"id":71,"cmd":"localhost.preview","folder":"Scratch","path":"alpha-report.md"}"#,
            None,
            Some(&routes),
        );
        let Answer::Now(held) = held else {
            panic!("a preview answers at once")
        };
        assert_eq!(held["ok"], serde_json::json!(true));
        assert_eq!(held["content"], serde_json::json!("the report\n"));
        assert_eq!(held["path"], serde_json::json!("alpha-report.md"));

        let (_, refused, _) = handle_command_with(
            r#"{"id":72,"cmd":"localhost.preview","folder":"Scratch","path":"missing.txt"}"#,
            None,
            Some(&routes),
        );
        let Answer::Now(refused) = refused else {
            panic!("a refusal answers at once")
        };
        assert_eq!(refused["ok"], serde_json::json!(false));
        assert_eq!(
            refused["error"],
            serde_json::json!("That folder is no longer granted. Search again.")
        );

        let (_, nothing, _) = handle_command_with(
            r#"{"id":73,"cmd":"localhost.preview","folder":"Scratch","path":"a.txt"}"#,
            None,
            None,
        );
        let Answer::Now(nothing) = nothing else {
            panic!("a window with no host answers at once")
        };
        assert_eq!(nothing["ok"], serde_json::json!(false));
    }

    /// The channel answers the page's one read from the same held state: the
    /// host polls its controller natively, so the page never touches the wire.
    #[test]
    fn localhost_snapshot_answers_from_the_held_state() {
        crate::pages::localhost_cache(crate::pages::HeldLocalHost {
            identity: "identity".into(),
            phase: "disconnected".into(),
            notice: "Start the local host to grant a folder.".into(),
            device: None,
            grants: Vec::new(),
            pending: Vec::new(),
            log: Vec::new(),
            ..Default::default()
        });

        let (id, answer, command) =
            handle_command_with(r#"{"id":81,"cmd":"localhost.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(81));
        assert_eq!(command, "localhost.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a held-state read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["phase"], "disconnected");
        assert_eq!(reply["grants"], serde_json::json!([]));
        assert_eq!(reply["pending"], serde_json::json!([]));
        assert_eq!(reply["notice"], "Start the local host to grant a folder.");
        assert!(reply["device"].is_null());
    }

    /// The shell's document carries the app's own page list, so the rail and
    /// drawer cannot offer a page the app does not have.
    /// The drawer's History is the panel's listing and its press is the
    /// panel's own open: both reach the routes by name, and a fresh chat is
    /// that open with no id.
    #[test]
    fn the_drawer_history_reads_and_opens_through_the_routes() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let opened: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));
        let slot = opened.clone();
        let routes = WebRoutes {
            history: Rc::new(|| {
                Ok(vec![crate::pages::HistoryRow {
                    id: "s-1".into(),
                    text: "📌 A calmer inbox (2 unread)".into(),
                    current: true,
                }])
            }),
            open_thread: Rc::new(move |id: Option<&str>| {
                slot.borrow_mut().push(id.map(str::to_owned));
                Ok(())
            }),
            ..stub_routes()
        };

        let (_, answer, command) =
            handle_command_with(r#"{"id":1,"cmd":"history.snapshot"}"#, None, Some(&routes));
        let Answer::Now(reply) = answer else {
            panic!("history is held state");
        };
        assert_eq!(command, "history.snapshot");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["rows"][0]["id"], "s-1");
        assert_eq!(reply["rows"][0]["text"], "📌 A calmer inbox (2 unread)");
        assert_eq!(reply["rows"][0]["current"], true);

        let (_, Answer::Now(reply), _) = handle_command_with(
            r#"{"id":2,"cmd":"history.open","thread":"s-2"}"#,
            None,
            Some(&routes),
        ) else {
            panic!("an open is answered now");
        };
        assert_eq!(reply["ok"], true);
        let (_, Answer::Now(reply), _) =
            handle_command_with(r#"{"id":3,"cmd":"chat.new"}"#, None, Some(&routes))
        else {
            panic!("a new chat is answered now");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(*opened.borrow(), vec![Some("s-2".to_owned()), None]);
    }

    /// A window without routes does not claim an empty account: the History
    /// read is refused in words, and the opens are refused with the rest.
    #[test]
    fn a_window_without_routes_cannot_read_history() {
        let (_, Answer::Now(reply), _) =
            handle_command_with(r#"{"id":1,"cmd":"history.snapshot"}"#, None, None)
        else {
            panic!("a read is answered now");
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "this window cannot read history");
    }

    /// The Settings page's flip reaches the app's own update route with the
    /// row's label and the word the page sent: the page picks nothing itself.
    #[test]
    fn the_settings_flip_reaches_the_app_with_its_row() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let seen: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let slot = seen.clone();
        let routes = WebRoutes {
            set_policy: Rc::new(move |label: &str, value: &str| {
                slot.borrow_mut().push((label.to_owned(), value.to_owned()));
                Ok(())
            }),
            ..stub_routes()
        };
        let (_, answer, command) = handle_command_with(
            r#"{"id":7,"cmd":"settings.policy.set","label":"protocol.files","value":"deny"}"#,
            None,
            Some(&routes),
        );
        assert_eq!(command, "settings.policy.set");
        assert_eq!(
            *seen.borrow(),
            vec![("protocol.files".to_owned(), "deny".to_owned())]
        );
        let Answer::Now(reply) = answer else {
            panic!("a flip is answered now");
        };
        assert_eq!(reply["ok"], true);
    }

    /// A refusal keeps the app's own sentence, so the page can print the reason
    /// the press did nothing rather than guessing at one.
    #[test]
    fn a_refused_flip_shows_the_apps_sentence() {
        use std::rc::Rc;
        let routes = WebRoutes {
            set_policy: Rc::new(|_: &str, _: &str| {
                Err("Another settings change is already in flight.".to_owned())
            }),
            ..stub_routes()
        };
        let (_, Answer::Now(reply), _) = handle_command_with(
            r#"{"id":8,"cmd":"settings.policy.set","label":"connector_default","value":"always_ask"}"#,
            None,
            Some(&routes),
        ) else {
            panic!("a refusal is answered now");
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(
            reply["error"],
            "Another settings change is already in flight."
        );
    }

    /// The Local host page's consent is data, not a claim: the page sends what
    /// its box says, and the host decides.
    #[test]
    fn the_host_start_carries_the_pages_consent() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let seen: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
        let slot = seen.clone();
        let routes = WebRoutes {
            host_start: Rc::new(move |consent: bool| {
                slot.borrow_mut().push(consent);
                Ok(())
            }),
            ..stub_routes()
        };
        let (_, Answer::Now(reply), command) = handle_command_with(
            r#"{"id":9,"cmd":"localhost.start","consent":true}"#,
            None,
            Some(&routes),
        ) else {
            panic!("a start is answered now");
        };
        assert_eq!(command, "localhost.start");
        assert_eq!(reply["ok"], true);
        assert_eq!(*seen.borrow(), vec![true]);
    }

    /// Reads stay reads: a window without routes cannot flip, reset, grant,
    /// start or stop, and says so instead of doing nothing.
    #[test]
    fn a_window_without_routes_cannot_write() {
        for raw in [
            r#"{"id":1,"cmd":"settings.policy.set","label":"connector_default","value":"auto_allow"}"#,
            r#"{"id":2,"cmd":"settings.connector.reset","connector":"notes"}"#,
            r#"{"id":3,"cmd":"localhost.grant","consent":true,"writable":false}"#,
            r#"{"id":4,"cmd":"localhost.start","consent":true}"#,
            r#"{"id":5,"cmd":"localhost.stop"}"#,
        ] {
            let (_, Answer::Now(reply), command) = handle_command_with(raw, None, None) else {
                panic!("{raw} is answered now");
            };
            assert_eq!(reply["ok"], false, "{command} is refused");
            assert_eq!(reply["error"], "this window cannot make that change");
        }
    }

    /// Upcoming's payload carries the two things its page cannot read: the run
    /// the last Tasks read named and the approvals the panel last painted --
    /// including the panel's "not read yet" state, which is not the same claim
    /// as "nothing waiting".
    #[test]
    fn upcoming_carries_the_next_run_and_the_unread_approvals() {
        crate::pages::tasks_cache(HeldTasks {
            next_run: Some(crate::pages::NextRun {
                title: "Nightly build".to_owned(),
                when: "in 2h".to_owned(),
            }),
            loaded: true,
            ..HeldTasks::default()
        });
        crate::pages::approvals_cache(crate::pages::ApprovalsView::default());
        let (id, answer, command) =
            handle_command_with(r#"{"id":71,"cmd":"upcoming.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(71));
        assert_eq!(command, "upcoming.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("upcoming answers from held state");
        };
        assert_eq!(reply["next_run"]["title"], "Nightly build");
        assert_eq!(reply["next_run"]["when"], "in 2h");
        assert_eq!(reply["schedules_loaded"], true);
        assert_eq!(reply["approvals"]["loaded"], false);
    }

    #[test]
    fn the_shell_lists_every_page_it_opens() {
        let doc = shell_document(None);
        assert!(doc.contains("\"page\":\"shell\""), "page named in boot");
        for page in crate::pages::PAGES {
            assert!(
                doc.contains(&format!("\"name\":\"{page}\"")),
                "the shell lists {page}"
            );
        }
        assert!(doc.contains("Local host"));
        assert!(!doc.contains("/*__BOOT__*/"));

        let (id, answer, command) =
            handle_command_with(r#"{"id":91,"cmd":"shell.snapshot"}"#, None, None);
        assert_eq!(id, serde_json::json!(91));
        assert_eq!(command, "shell.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("the shell's list is held state");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(
            reply["pages"].as_array().map(|pages| pages.len()),
            Some(crate::pages::PAGES.len())
        );
        assert_eq!(reply["pages"][0]["title"], "Chat");
        assert_eq!(reply["pages"][1]["name"], "threads");

        // The same window opened with routes carries the panel's History rows,
        // in the panel's words; without them the section is unread, not empty.
        let routes = WebRoutes {
            history: std::rc::Rc::new(|| {
                Ok(vec![crate::pages::HistoryRow {
                    id: "s-1".into(),
                    text: "(untitled)".into(),
                    current: false,
                }])
            }),
            ..stub_routes()
        };
        let with = shell_document(Some(&routes));
        assert!(with.contains("\"history\":[{"), "the rows ride in the boot");
        assert!(with.contains("\"(untitled)\""), "the row's own words");
        assert!(with.contains("\"id\":\"s-1\""));
        assert!(doc.contains("\"history\":null"), "no routes, no read");
    }

    /// Search runs the account read for the page's own query, and the answer
    /// updates the cache the native and web views share.
    #[test]
    fn search_run_reads_the_service_with_the_query() {
        use muse_account::BridgeConfig;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("muse-webui-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let service = serde_json::json!({
            "ok": true,
            "data": {
                "total": 1, "recognized": 1, "skipped": 0,
                "items": [{ "id": "thread-4", "title": "A conversation about parking" }]
            }
        })
        .to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("request");
            let mut writer = stream;
            writer.write_all(service.as_bytes()).expect("reply");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
            line
        });

        let client = AccountClient::new(BridgeConfig {
            python: "/nonexistent/python".into(),
            script: dir.join("bridge.py"),
            config_dir: dir.join("config"),
        })
        .use_host_socket(socket);

        let (_, answer, command) = handle_command_with(
            r#"{"id":62,"cmd":"search.run","query":"parking"}"#,
            Some(client),
            None,
        );
        assert_eq!(command, "search.run");
        let Answer::Later(receiver) = answer else {
            panic!("a search must leave the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker answers");
        let Fetched::Search(rows) = fetched else {
            panic!("the host answered with something else");
        };
        assert_eq!(rows.query, "parking");
        assert_eq!(rows.rows[0].id, "thread-4");

        let reply = reply_for(Fetched::Search(rows), &command, Instant::now());
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["rows"][0]["title"], "A conversation about parking");
        let cached = crate::pages::search_snapshot();
        assert_eq!(cached.query, "parking");
        assert_eq!(cached.rows[0].title, "A conversation about parking");

        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"search""#), "{request}");
        assert!(request.contains(r#""query":"parking""#), "{request}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_query_is_refused_without_a_read() {
        let (_, answer, command) =
            handle_command_with(r#"{"id":63,"cmd":"search.run","query":"   "}"#, None, None);
        assert_eq!(command, "search.run");
        let Answer::Now(reply) = answer else {
            panic!("an empty query must not start a read");
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "type words to search for first");
    }

    #[test]
    fn search_run_without_a_client_reports_the_missing_link() {
        let (_, answer, command) = handle_command_with(
            r#"{"id":64,"cmd":"search.run","query":"parking"}"#,
            None,
            None,
        );
        assert_eq!(command, "search.run");
        let Answer::Later(receiver) = answer else {
            panic!("a search must leave the main loop");
        };
        match receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers")
        {
            Fetched::Failed(error) => assert_eq!(error, "no account link in this app"),
            _ => panic!("no client, no rows"),
        }
    }

    #[test]
    fn the_hero_command_answers_a_data_uri_or_null() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");

        let (id, answer, command) = handle_command_with(
            r#"{"id":54,"cmd":"feed.hero","url":"https://example.test/never-cached.jpg"}"#,
            None,
            None,
        );
        assert_eq!(id, serde_json::json!(54));
        assert_eq!(command, "feed.hero");
        // The cache lookup and the file read leave the main loop: even a
        // local thumbnail read must not run on the UI thread.
        let Answer::Later(receiver) = answer else {
            panic!("the hero read must leave the main loop");
        };
        let fetched = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker answers");
        let Fetched::Hero(data) = fetched else {
            panic!("the hero read must answer a hero");
        };
        assert!(data.is_none(), "nothing cached, no data URI");
        let reply = reply_for(Fetched::Hero(data), &command, Instant::now());
        assert_eq!(reply["ok"], true);
        assert!(reply["data"].is_null());

        // Only http(s) artwork is ever offered: `feed_hero` goes through
        // `fetch_hero_cached`, which refuses other schemes before it looks
        // at the cache, and nothing here can reach the network.
        for url in [
            "file:///etc/passwd",
            "../muse-feed.json",
            "data:image/png;base64,iVBORw0KGgo=",
            "",
        ] {
            let (_, answer, _) = handle_command_with(
                &serde_json::json!({ "id": 55, "cmd": "feed.hero", "url": url }).to_string(),
                None,
                None,
            );
            let Answer::Later(receiver) = answer else {
                panic!("the hero read must leave the main loop");
            };
            let Fetched::Hero(data) = receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("the worker answers")
            else {
                panic!("the hero read must answer a hero");
            };
            assert!(data.is_none(), "{url} must never be served");
        }
    }
}
