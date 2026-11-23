//! Muse's shell window: the app's own pages in a Tauri 2 window.
//!
//! This is the port's window -- rail, page list, History and one of the app's
//! pages -- and it owns no state of its own. The app is the state layer and it
//! spawns this process, because the two cannot share one: the app is GTK 4 +
//! WebKitGTK 6.0 and this window is GTK 3 + WebKitGTK 4.1, and GTK aborts a
//! process that has both --
//!
//! ```text
//! Gtk-ERROR **: GTK 2/3 symbols detected. Using GTK 2/3 and GTK 4 in the same
//! process is not supported
//! ```
//!
//! The bridge is therefore the parent's pipes. This process's **stdout**
//! carries one `{"id","cmd"}` line per page command to the app; its **stdin**
//! carries the app's `{"id","reply"}` answers -- handed to the page through
//! `window.museReply(id, reply)`, so the page's ticket protocol is unchanged --
//! and three control lines: `{"raise":true}` when a second launch activates the
//! running instance, `{"boot":{...}}` when the app's own account read first
//! names the session, which the boot answer below was asked too early to carry,
//! and `{"nudge":"<page>"}` when a read landed that an open page shows -- the
//! page asks its own snapshot again, and no data or code crosses the pipe.
//! The app closing the pipe means the app is gone, and this window goes with
//! it.
//!
//! Run the app, not this binary: it has nothing to show on its own.

use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::time::Instant;

use serde_json::json;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// The committed bundle the app's own windows serve, boot data included.
const BUNDLE: &str = include_str!("../../muse/ui/built/app.html");
/// Where the document's boot data goes: the app's `webui::document` slot.
const BOOT_SLOT: &str = "/*__BOOT__*/";

/// The window's own clock, so its numbers line up with the app's `muse` lines.
static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
/// The page's first command, logged once.
static FIRST_COMMAND: AtomicBool = AtomicBool::new(false);
/// The document's boot payload. The app's first answer is a first paint, and
/// its own reads (the account name, the drawer's History) land after it, so the
/// app pushes a later payload which replaces this one and the drawer adopts it.
static BOOT: Mutex<Option<serde_json::Value>> = Mutex::new(None);

fn elapsed_ms() -> u128 {
    STARTED
        .get()
        .map(|start| start.elapsed().as_millis())
        .unwrap_or(0)
}

/// One command line to the app.
fn send(request: &serde_json::Value) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{request}")?;
    out.flush()
}

fn main() {
    let _ = STARTED.set(Instant::now());
    if std::io::stdin().is_terminal() {
        eprintln!(
            "muse-window: this window is started by the Muse app, which answers its pages; run the app instead of this binary."
        );
        std::process::exit(2);
    }
    let mut answers = BufReader::new(std::io::stdin());
    let boot = boot(&mut answers);
    if let Ok(mut slot) = BOOT.lock() {
        *slot = Some(boot);
    }
    // The reader runs on its own thread and needs the window handle, which only
    // exists once Tauri's setup has run; the channel is the hand-off.
    let (handle_tx, handle_rx) = channel::<AppHandle>();
    std::thread::spawn(move || {
        let Ok(app) = handle_rx.recv() else { return };
        read_loop(answers, app);
    });
    let build = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![muse])
        .register_uri_scheme_protocol("muse", move |_app, request| {
            let path = request.uri().path().to_owned();
            let started = Instant::now();
            let (status, body, content_type) = if path == "/shell" || path == "/" {
                let body = document();
                eprintln!(
                    "muse-window: document served at {}ms ({}us, {} bytes)",
                    elapsed_ms(),
                    started.elapsed().as_micros(),
                    body.len()
                );
                (200, body, "text/html")
            } else {
                (404, format!("no such document: {path}"), "text/plain")
            };
            http::Response::builder()
                .header("Content-Type", content_type)
                .header("Cache-Control", "no-store")
                .status(status)
                .body(body.into_bytes())
                .expect("a response the window can serve")
        })
        .setup(move |app| {
            let view = WebviewWindowBuilder::new(
                app,
                "shell",
                WebviewUrl::CustomProtocol(
                    tauri::Url::parse("muse://localhost/shell").expect("the window's own URL"),
                ),
            )
            .title("Muse")
            .inner_size(1100.0, 700.0)
            .initialization_script(SHIM)
            .build()?;
            eprintln!("muse-window: window built at {}ms", elapsed_ms());
            let _ = view.show();
            let _ = handle_tx.send(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!("tauri.conf.json"))
        .expect("the Tauri window builds");
    eprintln!("muse-window: event loop up at {}ms", elapsed_ms());
    build.run(|_app, _event| {});
}

/// The boot data the page needs before its first paint. Asked of the app, not
/// rebuilt here: the page list, the account and the History rows are the app's
/// answers, so the two surfaces cannot fork them.
fn boot(answers: &mut impl BufRead) -> serde_json::Value {
    if let Err(error) = send(&json!({ "id": 0, "cmd": "shell.snapshot" })) {
        eprintln!("muse-window: the app's bridge is not open: {error}");
        return boot_empty();
    }
    read_boot(answers)
}

/// The boot request's answer, however many lines it takes to reach. A raise or
/// noise before it is not a payload; the app's own push is one, and is newer
/// than the answer, so whichever arrives first completes the boot.
fn read_boot(answers: &mut impl BufRead) -> serde_json::Value {
    let mut line = String::new();
    loop {
        line.clear();
        match answers.read_line(&mut line) {
            Ok(0) => {
                eprintln!("muse-window: the app closed the bridge before answering");
                return boot_empty();
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("muse-window: cannot read the app's boot answer: {error}");
                return boot_empty();
            }
        }
        match parse(&line) {
            FromApp::Boot(boot) => return boot_with_page(boot),
            FromApp::Reply { id, reply } if id.as_i64() == Some(BOOT_TICKET) => {
                return boot_with_page(reply);
            }
            _ => {}
        }
    }
}

/// The ticket this window asks its boot under, before Tauri has started.
const BOOT_TICKET: i64 = 0;

/// A payload as the document paints it: the app's answer or push, tagged with
/// the page it belongs to. An answer that is not an object leaves the empty
/// shell rather than a payload nobody sent.
fn boot_with_page(payload: serde_json::Value) -> serde_json::Value {
    let mut boot = if payload.is_object() {
        payload
    } else {
        boot_empty()
    };
    boot["page"] = json!("shell");
    eprintln!("muse-window: boot from the app at {}ms", elapsed_ms());
    boot
}

fn boot_empty() -> serde_json::Value {
    json!({
        "page": "shell",
        "pages": [],
        "account": "",
        "build": "dev build",
        "history": null,
    })
}

/// The document, built from the boot payload as it is when the window asks for
/// it: a reload paints what the app holds now, not what it held at launch.
fn document() -> String {
    let boot = BOOT
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(boot_empty);
    // A title containing `</script>` must not close the data block; the app's
    // own document escapes the same way.
    BUNDLE.replace(BOOT_SLOT, &boot.to_string().replace("</", "<\\/"))
}

/// One line the app wrote to this window's stdin.
enum FromApp {
    /// A second launch activated the running instance.
    Raise,
    /// The app's own read landed: the drawer's payload, pushed.
    Boot(serde_json::Value),
    /// A read moved what an open page shows: ask that page to re-ask.
    Nudge(String),
    /// An answer to a page's command, by the page's own ticket.
    Reply {
        id: serde_json::Value,
        reply: serde_json::Value,
    },
    /// Not a message this build knows: a line from a newer or older app.
    Ignore,
}

/// Read one line as the message it is. The control lines come first because a
/// boot push and a raise carry no ticket, and an answer without one is not an
/// answer the page is waiting for.
fn parse(line: &str) -> FromApp {
    let Ok(answer) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return FromApp::Ignore;
    };
    if answer.get("raise").and_then(|raise| raise.as_bool()) == Some(true) {
        return FromApp::Raise;
    }
    if let Some(boot) = answer.get("boot") {
        return if boot.is_object() {
            FromApp::Boot(boot.clone())
        } else {
            FromApp::Ignore
        };
    }
    if let Some(page) = answer.get("nudge") {
        return match page.as_str() {
            Some(page) => FromApp::Nudge(page.to_owned()),
            None => FromApp::Ignore,
        };
    }
    let Some(id) = answer.get("id").cloned() else {
        return FromApp::Ignore;
    };
    let reply = answer
        .get("reply")
        .cloned()
        .unwrap_or_else(|| json!({ "ok": false, "error": "the app sent no reply" }));
    FromApp::Reply { id, reply }
}

/// The page's own callback, with the ticket it asked under: the protocol the
/// window keeps from every other surface.
fn reply_script(id: &serde_json::Value, reply: &serde_json::Value) -> String {
    format!("window.museReply && window.museReply({id}, {reply});")
}

/// The app's answers, and its control lines.
fn read_loop(mut answers: BufReader<std::io::Stdin>, app: AppHandle) {
    let mut line = String::new();
    loop {
        line.clear();
        match answers.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                eprintln!("muse-window: cannot read the app's answer: {error}");
                break;
            }
        }
        match parse(&line) {
            FromApp::Ignore => {}
            FromApp::Raise => {
                let window = app.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(view) = window.get_webview_window("shell") {
                        let _ = view.unminimize();
                        let _ = view.set_focus();
                    }
                });
            }
            // The app's own read landed: this is the drawer's payload, pushed
            // rather than asked for, so the page keeps no clock of its own.
            FromApp::Boot(boot) => adopt_boot(boot, &app),
            FromApp::Nudge(page) => {
                if let Some(script) = nudge_script(&page) {
                    let window = app.clone();
                    let _ = app.run_on_main_thread(move || {
                        if let Some(view) = window.get_webview_window("shell") {
                            let _ = view.eval(script);
                        }
                    });
                }
            }
            FromApp::Reply { id, reply } => {
                let script = reply_script(&id, &reply);
                let window = app.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(view) = window.get_webview_window("shell") {
                        let _ = view.eval(&script);
                    }
                });
            }
        }
    }
    eprintln!("muse-window: the app closed the bridge; closing the window");
    let exiting = app.clone();
    let _ = app.run_on_main_thread(move || exiting.exit(0));
}

/// The hook a nudge may reach, from a fixed table: the app names a page and
/// this side owns the script, so the pipe can ask a page to re-ask but can
/// never carry code. A name this build does not know is dropped, the way an
/// unknown control line is.
fn nudge_script(page: &str) -> Option<&'static str> {
    match page {
        "upcoming" => Some("window.museUpcomingReady && window.museUpcomingReady();"),
        _ => None,
    }
}

/// A pushed boot payload: kept for a reload, and handed to the drawer so the
/// account line and History move to the app's own read without one.
fn adopt_boot(reply: serde_json::Value, app: &AppHandle) {
    if !reply.is_object() {
        return;
    }
    let mut boot = reply;
    boot["page"] = json!("shell");
    eprintln!(
        "muse-window: boot pushed by the app at {}ms (account {:?}, {})",
        elapsed_ms(),
        boot.get("account")
            .and_then(|account| account.as_str())
            .unwrap_or_default(),
        match boot.get("history") {
            Some(serde_json::Value::Array(rows)) => format!("{} history rows", rows.len()),
            _ => "no history read".to_owned(),
        }
    );
    if let Ok(mut slot) = BOOT.lock() {
        *slot = Some(boot.clone());
    }
    let script = format!("window.museShellBoot && window.museShellBoot({boot});");
    let window = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(view) = window.get_webview_window("shell") {
            let _ = view.eval(&script);
        }
    });
}

/// The page's command, forwarded to the app: the same dispatcher, client and
/// routes its own windows use, so this is not a second implementation. The
/// answer arrives on stdin and reaches the page through `window.museReply`.
#[tauri::command]
fn muse(body: String) -> Result<(), String> {
    // The page hands over an already-serialised JSON string (`api.ts` stringifies
    // before `postMessage`), so it crosses untouched.
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| format!("bad command: {error}"))?;
    let command = parsed
        .get("cmd")
        .and_then(|cmd| cmd.as_str())
        .unwrap_or("?")
        .to_owned();
    if !FIRST_COMMAND.swap(true, Ordering::Relaxed) {
        eprintln!("muse-window: first command {command} at {}ms", elapsed_ms());
    }
    send(&parsed).map_err(|error| format!("muse-window: cannot reach the app: {error}"))
}

/// The page's protocol, unchanged: it still talks to `webkit.messageHandlers`,
/// and this shim carries that onto Tauri's IPC. The page does not learn which
/// window it is in.
const SHIM: &str = r#"
(function () {
  if (!window.__TAURI_INTERNALS__) { return; }
  const call = (body) => window.__TAURI_INTERNALS__.invoke("muse", { body });
  window.webkit = { messageHandlers: { muse: { postMessage: (body) => { call(body); } } } };
  window.museReplyTarget = null;
})();
"#;

#[cfg(test)]
mod tests {
    use super::{FromApp, boot_empty, nudge_script, parse, read_boot, reply_script};
    use serde_json::json;

    /// A raise is the app's control line, not one of a page's answers: it is
    /// acted on even when it arrives with a ticket of its own.
    #[test]
    fn a_raise_is_a_control_line() {
        assert!(matches!(parse(r#"{"raise":true}"#), FromApp::Raise));
        assert!(matches!(parse(r#"{"raise":true,"id":4}"#), FromApp::Raise));
        assert!(matches!(parse(r#"{"raise":false}"#), FromApp::Ignore));
        assert!(matches!(parse(r#"{"raise":"please"}"#), FromApp::Ignore));
    }

    /// A nudge names a page and carries nothing else: the window owns the hook
    /// it calls, so an unknown name or a non-string is dropped like any other
    /// line this build does not know.
    #[test]
    fn a_nudge_names_a_page_and_never_carries_code() {
        assert!(matches!(
            parse(r#"{"nudge":"upcoming"}"#),
            FromApp::Nudge(page) if page == "upcoming"
        ));
        assert!(matches!(parse(r#"{"nudge":4}"#), FromApp::Ignore));
        assert!(matches!(
            parse(r#"{"nudge":{"eval":"x"}}"#),
            FromApp::Ignore
        ));
        assert_eq!(
            nudge_script("upcoming"),
            Some("window.museUpcomingReady && window.museUpcomingReady();")
        );
        assert_eq!(nudge_script("chat"), None);
    }

    /// The app's push is its payload and nothing else: an object, adopted as a
    /// boot; a null or a string is a line this build does not know, and a page
    /// must not be handed it as one.
    #[test]
    fn a_boot_push_is_the_apps_payload() {
        let FromApp::Boot(boot) = parse(r#"{"boot":{"page":"shell","account":"Lotus"}}"#) else {
            panic!("a pushed object is a boot");
        };
        assert_eq!(boot["account"], "Lotus");
        assert!(matches!(parse(r#"{"boot":null}"#), FromApp::Ignore));
        assert!(matches!(parse(r#"{"boot":"later"}"#), FromApp::Ignore));
        assert!(matches!(parse(r#"{"boot":[1,2]}"#), FromApp::Ignore));
    }

    /// An answer keeps the page's ticket, and reaches the page through the
    /// callback it was asked under -- the protocol every other window uses.
    #[test]
    fn an_answer_keeps_the_pages_ticket() {
        let FromApp::Reply { id, reply } = parse(r#"{"id":12,"reply":{"ok":true,"rows":[]}}"#)
        else {
            panic!("a reply is an answer");
        };
        assert_eq!(id, json!(12));
        assert_eq!(reply["ok"], true);
        let script = reply_script(&id, &reply);
        assert!(
            script.starts_with("window.museReply && window.museReply(12, {"),
            "the ticket leads the call: {script}"
        );
        assert!(script.ends_with("});"), "and closes it: {script}");
    }

    /// An answer with no reply in it is answered in words the page can print,
    /// rather than leaving its ticket waiting for a message that never comes.
    #[test]
    fn an_answer_without_a_reply_is_refused_by_name() {
        let FromApp::Reply { id, reply } = parse(r#"{"id":5}"#) else {
            panic!("an answer without a reply is still an answer");
        };
        assert_eq!(id, json!(5));
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "the app sent no reply");
    }

    /// A line this build cannot read is not a message: not JSON, or JSON with
    /// nothing the window knows in it.
    #[test]
    fn a_line_this_build_does_not_know_is_ignored() {
        assert!(matches!(parse(""), FromApp::Ignore));
        assert!(matches!(parse("not json"), FromApp::Ignore));
        assert!(matches!(parse("[]"), FromApp::Ignore));
        assert!(matches!(
            parse(r#"{"cmd":"chat.snapshot"}"#),
            FromApp::Ignore
        ));
    }

    /// The boot completes on the app's own ticket, after whatever the pipe had
    /// to say first: control lines, junk and another page's answer are not the
    /// payload, and the window must not paint on them.
    #[test]
    fn the_boot_completes_on_the_apps_own_answer() {
        let mut answers = std::io::Cursor::new(
            "{\"raise\":true}\n\
             not json\n\
             {\"id\":1,\"reply\":{\"ok\":true}}\n\
             {\"id\":0,\"reply\":{\"pages\":[{\"name\":\"chat\",\"title\":\"Chat\"}],\"account\":\"Lotus\",\"history\":[]}}\n",
        );

        let boot = read_boot(&mut answers);

        assert_eq!(boot["page"], "shell", "the boot names its own page");
        assert_eq!(boot["account"], "Lotus");
        assert_eq!(boot["pages"][0]["name"], "chat");
        assert_eq!(boot["history"], json!([]));
    }

    /// An answer that is not an object is not a payload: the window paints the
    /// empty shell rather than a payload nobody sent.
    /// The app can push before its answer reaches the pipe -- the name lands
    /// between the request and the reply. The push is the same payload and the
    /// newer one, so it completes the boot instead of being dropped as noise.
    #[test]
    fn a_push_that_arrives_before_the_answer_is_the_boot() {
        let mut answers = std::io::Cursor::new(
            "{\"boot\":{\"pages\":[{\"name\":\"chat\",\"title\":\"Chat\"}],\"account\":\"Lotus\",\"history\":[]}}\n",
        );

        let boot = read_boot(&mut answers);

        assert_eq!(boot["page"], "shell");
        assert_eq!(boot["account"], "Lotus");
        assert_eq!(boot["pages"][0]["name"], "chat");
    }

    #[test]
    fn a_boot_answer_that_is_not_an_object_leaves_the_empty_shell() {
        let mut answers = std::io::Cursor::new("{\"id\":0,\"reply\":\"later\"}\n");
        assert_eq!(read_boot(&mut answers), boot_empty());
    }

    /// The app closing the pipe before answering is the same case: no payload,
    /// so the empty shell.
    #[test]
    fn a_closed_bridge_leaves_the_empty_shell() {
        let mut answers = std::io::Cursor::new("");
        assert_eq!(read_boot(&mut answers), boot_empty());
    }
}
