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

use crate::pages::{HeldGoals, HeldIdeas, HeldTasks};

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

/// What a fresh wire read produced off the main loop.
enum Fetched {
    Rows(Vec<ArtifactSummary>, u64),
    Goals(HeldGoals),
    Tasks(HeldTasks),
    Ideas(HeldIdeas),
    Failed(String),
}

/// A command's answer. Cache reads answer at once; a wire read answers when
/// its thread lands, so the window keeps painting during a slow listing.
enum Answer {
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

/// One command from the page. Reads only, and by name: an unknown command is
/// refused instead of guessed at, so a typo in the UI is visible in the page
/// and in the log.
fn handle_command(raw: &str, client: Option<AccountClient>) -> (serde_json::Value, Answer, String) {
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
        other => Answer::Now(
            serde_json::json!({ "ok": false, "error": format!("unknown command: {other}") }),
        ),
    };
    (id, answer, command)
}

/// The reply a worker's answer becomes, and the cache it leaves behind: the
/// native page, the boot payload and this reply tell the same story
/// afterwards. Pure over the answer, so a test can read it without a window.
fn reply_for(fetched: Fetched, command: &str, started: Instant) -> serde_json::Value {
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

/// Open one page in its own window on the rows the native page last read.
/// Returns false when this build cannot render a WebView.
fn open_window(
    title: &str,
    document: String,
    client: Option<AccountClient>,
    probe: &'static str,
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
    manager.connect_script_message_received(None, move |_, value| {
        let raw = value.to_str().to_string();
        let (id, answer, command) = handle_command(&raw, client.clone());
        eprintln!("muse webui: command {command}");
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

/// Open the Library page in its own window, on the rows the native page last
/// read.
pub(crate) fn open_library_window(
    artifacts: Vec<ArtifactSummary>,
    total: u64,
    client: Option<AccountClient>,
) -> bool {
    open_window(
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
        "Ideas · web view",
        ideas_document(&held),
        client,
        IDEAS_PROBE,
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
        assert!(doc.contains("--accent: #36a3ff"), "stylesheet inlined");
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
        let (id, answer, command) = handle_command(r#"{"id":7,"cmd":"library.snapshot"}"#, None);
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
        let (id, answer, command) = handle_command(r#"{"id":9,"cmd":"goals.snapshot"}"#, None);
        assert_eq!(id, serde_json::json!(9));
        assert_eq!(command, "goals.snapshot");
        let Answer::Now(reply) = answer else {
            panic!("a cache read must answer without a thread");
        };
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["goals"][0]["title"], "Held goal");
        assert_eq!(reply["tracking"][0]["title"], "Held tracking");
        assert_eq!(reply["goals_total"], 1);

        let (_, answer, command) = handle_command(r#"{"id":8,"cmd":"library.delete"}"#, None);
        assert_eq!(command, "library.delete");
        let Answer::Now(reply) = answer else {
            panic!("an unknown command must be refused at once");
        };
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "unknown command: library.delete");

        let (id, answer, _) = handle_command("not json", None);
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
            handle_command(r#"{"id":3,"cmd":"library.refresh"}"#, Some(client));
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
        }
        let request = server.join().expect("server");
        assert!(request.contains(r#""op":"artifacts""#), "{request}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_is_a_wire_read_and_reports_a_missing_link() {
        let (id, answer, command) = handle_command(r#"{"id":9,"cmd":"library.refresh"}"#, None);
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
            handle_command(r#"{"id":12,"cmd":"goals.refresh"}"#, Some(client));
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
        let (_, answer, command) = handle_command(r#"{"id":13,"cmd":"goals.refresh"}"#, None);
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
        let (id, answer, command) = handle_command(r#"{"id":21,"cmd":"tasks.snapshot"}"#, None);
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
            handle_command(r#"{"id":22,"cmd":"tasks.refresh"}"#, Some(client));
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
        let (_, answer, command) = handle_command(r#"{"id":23,"cmd":"tasks.refresh"}"#, None);
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
        let (id, answer, command) = handle_command(r#"{"id":31,"cmd":"ideas.snapshot"}"#, None);
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
            handle_command(r#"{"id":32,"cmd":"ideas.refresh"}"#, Some(client));
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
        let (_, answer, command) = handle_command(r#"{"id":33,"cmd":"ideas.refresh"}"#, None);
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
}
