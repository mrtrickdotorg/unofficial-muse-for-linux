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
        "loaded": held.loaded,
        "error": held.error,
    })
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

fn localhost_reply(held: &HeldLocalHost) -> serde_json::Value {
    let mut reply = localhost_payload(held);
    reply["ok"] = serde_json::json!(true);
    reply
}

/// The shell page: the rail and its page list, plus the account and build
/// stamp the native drawer prints. Each page inside the shell asks its own
/// snapshot, so the shell carries no page data of its own.
fn shell_payload(routes: Option<&WebRoutes>) -> serde_json::Value {
    serde_json::json!({
        "pages": crate::pages::shell_pages(),
        "account": crate::pages::chat_snapshot().account,
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

fn shell_document(routes: Option<&WebRoutes>) -> String {
    let mut boot = shell_payload(routes);
    boot["page"] = serde_json::json!("shell");
    document(&boot)
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
    Ideas(HeldIdeas),
    Threads(HeldThreads),
    Feed(HeldFeed),
    /// A cached hero's data URI, or None when nothing is cached.
    Hero(Option<String>),
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
pub(crate) type WebHistory = std::rc::Rc<dyn Fn() -> Result<Vec<crate::pages::HistoryRow>, String>>;
pub(crate) type WebOpenThread = std::rc::Rc<dyn Fn(Option<&str>) -> Result<(), String>>;
pub(crate) type WebSetPolicy = std::rc::Rc<dyn Fn(&str, &str) -> Result<(), String>>;
pub(crate) type WebResetConnector = std::rc::Rc<dyn Fn(&str) -> Result<(), String>>;
pub(crate) type WebHostGrant = std::rc::Rc<dyn Fn(bool, bool) -> Result<(), String>>;
pub(crate) type WebHostStart = std::rc::Rc<dyn Fn(bool) -> Result<(), String>>;
pub(crate) type WebHostStop = std::rc::Rc<dyn Fn() -> Result<(), String>>;

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
    /// Ask for a folder grant. The chooser is the app's own native dialog;
    /// the page gets the outcome from its next snapshot.
    pub host_grant: WebHostGrant,
    /// Start the local host, carrying the page's consent.
    pub host_start: WebHostStart,
    /// Stop the host and revoke its grants.
    pub host_stop: WebHostStop,
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
            Answer::Now(serde_json::json!({ "ok": true, "artifacts": artifacts, "total": total }))
        }
        "library.refresh" => Answer::Later(fresh_listing(client)),
        "goals.snapshot" => {
            let held = crate::pages::goals_snapshot();
            Answer::Now(serde_json::json!({
                "ok": true,
                "goals": held.goals,
                "goals_total": held.goals_total,
                "tracking": held.tracking,
                "tracking_total": held.tracking_total,
                "tracking_error": held.tracking_error,
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
        "ideas.snapshot" => Answer::Now(ideas_reply(&crate::pages::ideas_snapshot())),
        "ideas.refresh" => Answer::Later(fresh_ideas(client)),
        "threads.snapshot" => Answer::Now(threads_reply(&crate::pages::threads_snapshot())),
        "threads.refresh" => Answer::Later(fresh_threads(client)),
        "feed.snapshot" => Answer::Now(feed_reply(&crate::pages::feed_snapshot())),
        "feed.refresh" => Answer::Later(fresh_feed(client)),
        // Settings answers from held state only: the native page owns the
        // presses that write, and the policy is whatever the chat panel last
        // read rather than a second trip here.
        "settings.snapshot" => Answer::Now(settings_reply(&crate::pages::settings_snapshot())),
        // The host polls its own controller every 250 ms and the native paint
        // re-projects each time, so this answer follows that pump: the page
        // re-asks and sees new cards and log lines without holding a lock.
        "localhost.snapshot" => Answer::Now(localhost_reply(&crate::pages::localhost_snapshot())),
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
            let consent = request
                .get("consent")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let writable = request
                .get("writable")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            route_answer(routes.map(|routes| (routes.host_grant)(consent, writable)))
        }
        "localhost.start" => {
            let consent = request
                .get("consent")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            route_answer(routes.map(|routes| (routes.host_start)(consent)))
        }
        "localhost.stop" => route_answer(routes.map(|routes| (routes.host_stop)())),
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

/// The reply a worker's answer becomes, and the cache it leaves behind: the
/// native page, the boot payload and this reply tell the same story
/// afterwards. Pure over the answer, so a test can read it without a window.
pub(crate) fn reply_for(fetched: Fetched, command: &str, started: Instant) -> serde_json::Value {
    match fetched {
        Fetched::Rows(artifacts, total) => {
            crate::pages::library_cache(artifacts.clone(), total);
            eprintln!(
                "muse webui: {command} read the service: rows={} total={} ms={}",
                artifacts.len(),
                total,
                started.elapsed().as_millis()
            );
            serde_json::json!({ "ok": true, "artifacts": artifacts, "total": total })
        }
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
            crate::pages::tasks_cache(held);
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
    /// The window a page has open, if any: a native paint can nudge it by name.
    /// Weak, so a window the reader closed simply stops upgrading and needs no
    /// cleanup here.
    static OPEN_WINDOWS: std::cell::RefCell<
        std::collections::HashMap<&'static str, gtk4::glib::WeakRef<webkit6::WebView>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn track_window(page: &'static str, view: &webkit6::WebView) {
    use webkit6::prelude::*;
    let weak = view.downgrade();
    OPEN_WINDOWS.with(|windows| {
        windows.borrow_mut().insert(page, weak);
    });
}

/// Tell the open window for `page` that a native paint changed what it shows.
/// The page re-asks its own snapshot; nothing is pushed as data, so the cache
/// the paint wrote stays the only source of truth.
pub(crate) fn nudge_page(page: &'static str, hook: &str) {
    OPEN_WINDOWS.with(|windows| {
        let Some(view) = windows.borrow().get(page).and_then(|weak| weak.upgrade()) else {
            return;
        };
        use webkit6::prelude::*;
        let script = format!("window.{hook} && window.{hook}();");
        view.evaluate_javascript(&script, None, None, gtk4::gio::Cancellable::NONE, |_| {});
    });
}

/// The policy projection changed: an open Settings window should re-ask, since
/// the chat panel's read lands after that window opened.
pub(crate) fn settings_policy_changed() {
    nudge_page("settings", "musePolicyReady");
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
    // Named commands so far, per window: see `should_log_command`.
    let named: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new()));
    manager.connect_script_message_received(None, move |_, value| {
        let raw = value.to_str().to_string();
        let (id, answer, command) = handle_command_with(&raw, client.clone(), routes.as_ref());
        if should_log_command(&command, &mut named.borrow_mut()) {
            eprintln!("muse webui: command {command}");
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
    view.connect_load_changed(move |view, event| {
        if event != webkit6::LoadEvent::Finished {
            return;
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
            Fetched::Feed(..) => panic!("a library read must not answer with a feed"),
            Fetched::Hero(..) => panic!("a library read must not answer with a hero"),
            Fetched::Search(..) => panic!("a library read must not answer with a search"),
        }
        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"artifacts""#), "{request}");
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
            Fetched::Feed(..) => panic!("no client, no feed"),
            Fetched::Hero(..) => panic!("no client, no hero"),
            Fetched::Search(..) => panic!("no client, no search"),
        }
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

    /// Chat boot: one page-tagged payload with the native transcript's own
    /// rows -- day dividers, bubbles and notices -- and its thread title.
    /// Every route the Chat window has, stubbed: a test binds only the ones
    /// it is about, so a new control cannot quietly break the others' tests.
    fn stub_routes() -> WebRoutes {
        use std::rc::Rc;
        WebRoutes {
            send: Rc::new(|_: &str, _: Option<WebQuote>| Ok(())),
            stop: Rc::new(|| Ok(())),
            attach: Rc::new(|| Ok(())),
            unstage: Rc::new(|_: &str| Ok(())),
            approvals: Rc::new(|| Ok(())),
            decide: Rc::new(|_: &str, _: &str| Ok(())),
            history: Rc::new(|| Ok(Vec::new())),
            open_thread: Rc::new(|_: Option<&str>| Ok(())),
            set_policy: Rc::new(|_: &str, _: &str| Ok(())),
            reset_connector: Rc::new(|_: &str| Ok(())),
            host_grant: Rc::new(|_: bool, _: bool| Ok(())),
            host_start: Rc::new(|_: bool| Ok(())),
            host_stop: Rc::new(|| Ok(())),
        }
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

    /// Settings opens on the app's own held state: the updater's last answer
    /// and the policy the chat panel read, with the page named for the bundle.
    #[test]
    fn the_settings_document_names_its_page_and_carries_the_held_state() {
        crate::pages::settings_updates_cache(crate::pages::UpdatesView {
            appimage: true,
            status: "Up to date (build 62ce6e3).".into(),
            busy: false,
            checked: true,
            staged: false,
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
            checked: true,
            staged: true,
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

    /// The Local host page opens on the host's read model, named for the
    /// bundle, so a fresh window paints the same grants, cards and log the
    /// native section already shows.
    #[test]
    fn the_localhost_document_names_its_page_and_carries_the_held_state() {
        crate::pages::localhost_cache(crate::pages::HeldLocalHost {
            identity: "This machine registers as its own device.".into(),
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
                operation: "create_text".into(),
                path: "Scratch/notes.txt".into(),
                target: None,
                preview: Some("hello from the agent".into()),
                deciding: false,
                countdown: "Expires in 42s".into(),
            }],
            log: vec!["files.read_text Scratch/notes.txt → allowed".into()],
        });

        let held = crate::pages::localhost_snapshot();
        let doc = localhost_document(&held);
        assert!(doc.contains("\"page\":\"localhost\""), "page named in boot");
        assert!(doc.contains("Listening for the account's own node commands."));
        assert!(doc.contains("create_text"));
        assert!(doc.contains("hello from the agent"));
        assert!(!doc.contains("/*__BOOT__*/"));
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
