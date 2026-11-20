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

use crate::pages::HeldGoals;

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

/// What a fresh wire read produced off the main loop.
enum Fetched {
    Rows(Vec<ArtifactSummary>, u64),
    Goals(HeldGoals, Option<String>),
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
                    let tracking_error = held.tracking_error.clone();
                    Fetched::Goals(held, tracking_error)
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
        Fetched::Goals(held, tracking_error) => {
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
                "tracking_error": tracking_error,
            })
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
        let Fetched::Goals(rows, tracking_error) = fetched else {
            panic!("the host answered with something else");
        };
        assert_eq!(rows.goals[0].title, "Read from the service");
        assert_eq!(rows.goals_total, 40);
        assert_eq!(rows.tracking[0].title, "Tracked from the service");
        assert_eq!(rows.tracking_total, 3);
        assert!(tracking_error.is_none());

        let reply = reply_for(
            Fetched::Goals(rows, tracking_error),
            &command,
            Instant::now(),
        );
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

    /// The wait is a timer, not an idle handler: an idle source would run the
    /// poll back to back and hold a core for a 27 s listing.
    #[test]
    fn the_answer_wait_is_a_timed_poll() {
        assert!(ANSWER_POLL >= Duration::from_millis(50));
        assert!(ANSWER_POLL <= Duration::from_millis(500));
    }
}
