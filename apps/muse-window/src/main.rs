//! S4 window spike: Muse's shell document in a Tauri 2 window.
//!
//! The window is what the port's last question is about, so this binary is a
//! window and nothing else: Tauri owns it, wry serves the app's own shell
//! document from the committed bundle over `muse://`, and every command the
//! page sends crosses back to the running app, which answers with the existing
//! [`handle_command_with`] against the state its window has actually painted.
//! The bridge is `spike-cmd`: one JSON line per connection on the Unix socket
//! `MUSE_SPIKE_CMD` names, installed by the app with `--features spike-cmd`.
//!
//! Why two processes: the app is GTK 4 + WebKitGTK 6.0 and this window is GTK 3
//! + WebKitGTK 4.1, and GTK aborts a process that has both --
//!
//! ```text
//! Gtk-ERROR **: GTK 2/3 symbols detected. Using GTK 2/3 and GTK 4 in the same
//! process is not supported
//! ```
//!
//! -- measured on this seat with the state layer initialised first. So the
//! window cannot be swapped into the app's process: the split is one hop over a
//! socket, and its price is what the probe logs below measure.
//!
//! Not shipped. Debug-only, like the seam it talks to.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::json;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// The same committed bundle the app's own windows serve.
const BUNDLE: &str = include_str!("../../muse/ui/built/app.html");
/// Where the document's boot data goes: the app's `webui::document` slot.
const BOOT_SLOT: &str = "/*__BOOT__*/";

/// The window's own clock, so its numbers line up with the GTK window's
/// `muse webui:` lines instead of being argued about.
static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
/// The page's first command, logged once.
static FIRST_COMMAND: AtomicBool = AtomicBool::new(false);

fn elapsed_ms() -> u128 {
    STARTED
        .get()
        .map(|start| start.elapsed().as_millis())
        .unwrap_or(0)
}

fn socket() -> Option<String> {
    std::env::var("MUSE_SPIKE_CMD")
        .ok()
        .filter(|path| !path.is_empty())
}

/// One command to the app, one answer back. Each call is its own connection so
/// a slow read on the app's side cannot queue the next question behind it.
fn ask_app(stream: &mut UnixStream, request: &serde_json::Value) -> Option<serde_json::Value> {
    stream.write_all(request.to_string().as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    let mut line = String::new();
    BufReader::new(&mut *stream).read_line(&mut line).ok()?;
    let answered: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    answered.get("reply").cloned()
}

fn connect() -> Option<UnixStream> {
    let path = socket()?;
    match UnixStream::connect(&path) {
        Ok(stream) => Some(stream),
        Err(error) => {
            eprintln!("muse-window: no app on {path}: {error}");
            None
        }
    }
}

/// The boot data the page needs before its first paint. Asked of the app, not
/// rebuilt here: the page list, the account and the history row are the app's
/// answers, so the two surfaces cannot fork them.
fn boot(stream: &mut UnixStream) -> serde_json::Value {
    let mut boot =
        ask_app(stream, &json!({"id": 0, "cmd": "shell.snapshot"})).unwrap_or_else(|| {
            json!({
                "pages": [],
                "account": "",
                "build": "dev build",
                "history": null,
            })
        });
    boot["page"] = json!("shell");
    boot
}

fn document() -> String {
    let Some(mut stream) = connect() else {
        let boot = boot_empty();
        return BUNDLE.replace(BOOT_SLOT, &boot.to_string().replace("</", "<\\/"));
    };
    let boot = boot(&mut stream);
    // A title containing `</script>` must not close the data block; the app's
    // own document escapes the same way.
    BUNDLE.replace(BOOT_SLOT, &boot.to_string().replace("</", "<\\/"))
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

/// The page's command, answered by the app: the same dispatcher, client and
/// routes its own Chat window uses, so this is not a second implementation.
#[tauri::command]
async fn muse(app: AppHandle, body: String) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| format!("bad command: {error}"))?;
    let id = parsed.get("id").cloned().unwrap_or(json!(0));
    let command = parsed
        .get("cmd")
        .and_then(|cmd| cmd.as_str())
        .unwrap_or("?")
        .to_owned();
    if !FIRST_COMMAND.swap(true, Ordering::Relaxed) {
        eprintln!("muse-window: first command {command} at {}ms", elapsed_ms());
    }
    let started = Instant::now();
    let reply = match connect() {
        Some(mut stream) => ask_app(&mut stream, &parsed).unwrap_or_else(
            || json!({"ok": false, "error": "muse-window: the app sent no answer"}),
        ),
        None => json!({
            "ok": false,
            "error": "muse-window: no app on the bridge socket; start it with MUSE_SPIKE_CMD"
        }),
    };
    eprintln!(
        "muse-window: answered {command} in {}us ({}ms from start)",
        started.elapsed().as_micros(),
        elapsed_ms()
    );
    let script = format!("window.museReply && window.museReply({}, {});", id, reply);
    if let Some(view) = app.get_webview_window("shell") {
        let _ = view.eval(&script);
    }
    Ok(())
}

/// The page's protocol, unchanged: it still talks to `webkit.messageHandlers`,
/// and this shim carries that onto Tauri's IPC. Keeping the page's side exactly
/// as the GTK window has it is the point -- the page does not learn which window
/// it is in.
const SHIM: &str = r#"
(function () {
  if (!window.__TAURI_INTERNALS__) { return; }
  // The page hands over an already-serialised JSON string (`api.ts` stringifies
  // before `postMessage`), so it goes through untouched.
  const call = (body) => window.__TAURI_INTERNALS__.invoke("muse", { body });
  window.webkit = { messageHandlers: { muse: { postMessage: (body) => { call(body); } } } };
  window.museReplyTarget = null;
})();
"#;

fn main() {
    let _ = STARTED.set(Instant::now());
    let build = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![muse])
        .register_uri_scheme_protocol("muse", |_app, request| {
            let path = request.uri().path().to_owned();
            let tracing_started = Instant::now();
            let (status, body, content_type) = if path == "/shell" || path == "/" {
                let document = document();
                eprintln!(
                    "muse-window: document served at {}ms ({}us to build, {} bytes)",
                    elapsed_ms(),
                    tracing_started.elapsed().as_micros(),
                    document.len()
                );
                (200, document, "text/html")
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
        .setup(|app| {
            let view = WebviewWindowBuilder::new(
                app,
                "shell",
                WebviewUrl::CustomProtocol(
                    tauri::Url::parse("muse://localhost/shell").expect("the window's own URL"),
                ),
            )
            .title("Muse · shell (Tauri)")
            .inner_size(1040.0, 760.0)
            .initialization_script(SHIM)
            .build()?;
            eprintln!("muse-window: window built at {}ms", elapsed_ms());
            let _ = view.show();
            Ok(())
        })
        .build(tauri::generate_context!("tauri.conf.json"))
        .expect("the Tauri window builds");
    eprintln!("muse-window: event loop up at {}ms", elapsed_ms());
    build.run(|_app, _event| {});
}
