//! The app's side of the shell window: the app's pages in a Tauri window,
//! answered from this process over the parent's pipes.
//!
//! The port's end state is a React document in a Tauri window with this process
//! behind it as the state layer. They cannot share a process: this app is
//! GTK 4 + WebKitGTK 6.0 and the window is GTK 3 + WebKitGTK 4.1, and GTK
//! aborts a process that has both --
//!
//! ```text
//! Gtk-ERROR **: GTK 2/3 symbols detected. Using GTK 2/3 and GTK 4 in the same
//! process is not supported
//! ```
//!
//! -- so the window is a child process and the bridge is its own stdio. The
//! window writes one `{"id","cmd"}` line per page command to its stdout; this
//! process answers with one `{"id","reply"}` line on the child's stdin, through
//! [`crate::webui::handle_command_with`], against the state the app holds. The
//! same pipe carries the app's own control lines: `{"raise":true}` when a
//! second launch activates the running instance, and `{"boot":{...}}` when the
//! chat cache first names the account, which the window's first paint could not
//! know.
//!
//! A pipe rather than a socket on purpose: only the child this process spawned
//! holds the other end, so the page's command surface -- sends, decisions,
//! grants -- is not reachable by any other local process.
//!
//! The native GTK window stays the fallback and the reference: no window
//! binary, no usable saved link, or `MUSE_SHELL=gtk` all open it instead.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use muse_account::AccountClient;

use crate::webui::{Answer, WebRoutes, handle_command_with, reply_for};

/// How often a worker's answer is looked at, the same cadence the windows use.
const ANSWER_POLL: Duration = Duration::from_millis(100);

/// Whether this launch opens the Tauri window. Decided once, because both the
/// shell and the activation handler ask, and the answer must not move under
/// them.
static MODE: OnceLock<bool> = OnceLock::new();

/// A window that started and died before answering anything leaves nothing on
/// screen: the app falls back to the native one and stays there.
static FALLEN_BACK: AtomicBool = AtomicBool::new(false);
/// Whether the window has answered at least one page command.
static ANSWERED: AtomicBool = AtomicBool::new(false);
/// Whether the drawer has been pushed the payload its first paint lacked.
static PUSHED: AtomicBool = AtomicBool::new(false);

/// What a page's commands are answered with: the same client and routes a
/// native window is opened with, installed where the chat panel is reachable.
struct Source {
    client: Option<AccountClient>,
    routes: WebRoutes,
}

thread_local! {
    static SOURCE: std::cell::RefCell<Option<Source>> = const { std::cell::RefCell::new(None) };
    /// The child's stdin: replies and control lines go here, always from the
    /// main thread. Boxed rather than the concrete pipe so the tests can stand
    /// a recorder in its place and read back the protocol.
    static WRITER: std::cell::RefCell<Option<Box<dyn Write>>> =
        const { std::cell::RefCell::new(None) };
    /// Quit the app when the window goes: with the native window hidden, the
    /// shell window is the only surface, so its closing is the app's closing.
    static QUIT: std::cell::RefCell<Option<Rc<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
    /// What to show instead when the window never came up.
    static FALLBACK: std::cell::RefCell<Option<Rc<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether the app opens the Tauri shell window instead of the native one.
pub(crate) fn tauri() -> bool {
    *MODE.get_or_init(choose) && !FALLEN_BACK.load(Ordering::Relaxed)
}

/// `MUSE_SHELL` picks the window; unset means "the shell window when it can
/// be". A window that cannot be opened never leaves the reader without one:
/// every refusal logs its reason and falls back to GTK.
fn choose() -> bool {
    match std::env::var("MUSE_SHELL")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "gtk" => {
            eprintln!("muse: MUSE_SHELL=gtk: opening the native window");
            return false;
        }
        "tauri" => {}
        "" => {}
        other => {
            eprintln!("muse: MUSE_SHELL={other} is neither gtk nor tauri; choosing automatically")
        }
    }
    // Only a linked account has a shell to show: the sign-in surface is still
    // the native one, so an unlinked clone keeps the window that can carry it.
    if crate::session::load_saved_link()
        .filter(|link| link.consent)
        .is_none()
    {
        eprintln!(
            "muse: no usable saved link: sign-in is still the native surface, opening the GTK window"
        );
        return false;
    }
    let Some(binary) = window_binary() else {
        eprintln!(
            "muse: no shell window binary found (build apps/muse-window, or set MUSE_WINDOW_BIN); opening the GTK window"
        );
        return false;
    };
    eprintln!("muse: opening the shell window {}", binary.display());
    true
}

/// The window binary: `MUSE_WINDOW_BIN`, else beside this executable, else the
/// dev checkout's own build of the separate project (GTK 3 must not join this
/// workspace's lock, build or tests, so it has its own target directory).
pub(crate) fn window_binary() -> Option<PathBuf> {
    if let Some(asked) = std::env::var_os("MUSE_WINDOW_BIN").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(asked);
        if path.is_file() {
            return Some(path);
        }
        eprintln!(
            "muse: MUSE_WINDOW_BIN does not name a file: {}",
            path.display()
        );
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    candidates(&exe)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn candidates(exe: &Path) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    vec![
        dir.join("muse-window"),
        dir.join("../../apps/muse-window/target/debug/muse-window"),
        dir.join("../../apps/muse-window/target/release/muse-window"),
    ]
}

/// Record what the shell window's commands are answered with. Called where the
/// chat panel is built, so a page press reaches the same sender, chooser and
/// decision path a native press does.
pub(crate) fn install(client: Option<AccountClient>, routes: WebRoutes) {
    SOURCE.with(|slot| *slot.borrow_mut() = Some(Source { client, routes }));
}

/// Start the window and answer it. False means no window was started, with the
/// reason logged, so the caller can still present the native one.
pub(crate) fn launch(quit: Rc<dyn Fn()>, fallback: Rc<dyn Fn()>) -> bool {
    if !tauri() {
        return false;
    }
    let Some(binary) = window_binary() else {
        return false;
    };
    let mut command = Command::new(&binary);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // The seat's display and session are the window's too; what this process
    // added for its own WebKit is not. A library path naming the GTK 4 bundle
    // would hand the window the other WebKit generation, and GTK aborts when
    // both meet in one process.
    command.env_remove("GTK_THEME");
    command.env_remove("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS");
    command.env_remove("WEBKIT_FORCE_SANDBOX");
    if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
        match clean_library_path(&path.to_string_lossy()) {
            Some(kept) => command.env("LD_LIBRARY_PATH", kept),
            None => command.env_remove("LD_LIBRARY_PATH"),
        };
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "muse: cannot start the shell window {}: {error}",
                binary.display()
            );
            return false;
        }
    };
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        eprintln!("muse: the shell window has no bridge");
        let _ = child.kill();
        return false;
    };
    WRITER.with(|slot| *slot.borrow_mut() = Some(Box::new(stdin)));
    QUIT.with(|slot| *slot.borrow_mut() = Some(quit));
    FALLBACK.with(|slot| *slot.borrow_mut() = Some(fallback));
    // The child's commands arrive on its stdout, off the main loop's thread;
    // each is answered on the main loop, where the state it reads lives.
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let request = line.trim().to_owned();
            if request.is_empty() {
                continue;
            }
            gtk4::glib::MainContext::default().invoke(move || serve(&request));
        }
        gtk4::glib::MainContext::default().invoke(window_gone);
    });
    true
}

/// Raise the window a second launch activated: one control line, and the window
/// brings itself forward.
pub(crate) fn raise() {
    if !tauri() {
        return;
    }
    eprintln!("muse: raising the shell window");
    write(&serde_json::json!({ "raise": true }));
}

/// The chat cache has named the account: push the window a payload the app's
/// own read fills, once. The window's boot answer is its first paint, asked
/// before that read landed, so this is the line that closes the gap -- the app
/// knows when the name arrives, while the window could only guess by asking
/// again.
pub(crate) fn account_named() {
    if !tauri() {
        return;
    }
    push_drawer_boot();
}

/// The push itself, without the mode gate: the payload the drawer's first paint
/// could not carry, built from the state the app holds now, and written once.
/// Nothing is lost when there is no window yet -- that launch's own boot request
/// is asked after this read and carries the name itself.
fn push_drawer_boot() {
    if PUSHED.load(Ordering::Relaxed) || !WRITER.with(|slot| slot.borrow().is_some()) {
        return;
    }
    let boot = SOURCE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|source| crate::webui::shell_boot(Some(&source.routes)))
    });
    let Some(boot) = boot else {
        return;
    };
    PUSHED.store(true, Ordering::Relaxed);
    eprintln!(
        "muse: the account is named ({}); pushing the shell drawer its boot payload",
        boot.get("account")
            .and_then(|account| account.as_str())
            .unwrap_or_default()
    );
    write(&serde_json::json!({ "boot": boot }));
}

/// Answer one page command. On the main thread by construction: the held rows
/// are thread-locals of the thread that painted them.
fn serve(raw: &str) {
    ANSWERED.store(true, Ordering::Relaxed);
    let started = Instant::now();
    let (id, answer, command) = SOURCE.with(|slot| match slot.borrow().as_ref() {
        Some(source) => handle_command_with(raw, source.client.clone(), Some(&source.routes)),
        None => (
            // No state to answer from, but the page is still waiting on its own
            // ticket: a refusal under the wrong id would leave it waiting.
            ticket(raw),
            Answer::Now(serde_json::json!({
                "ok": false,
                "error": "the app has no page state yet"
            })),
            "?".to_owned(),
        ),
    });
    match answer {
        Answer::Now(reply) => {
            eprintln!(
                "muse shell: {command} answered in {}us",
                started.elapsed().as_micros()
            );
            write(&serde_json::json!({ "id": id, "reply": reply }));
        }
        Answer::Later(pending) => {
            gtk4::glib::timeout_add_local(ANSWER_POLL, move || match pending.try_recv() {
                Ok(fetched) => {
                    let reply = reply_for(fetched, &command, started);
                    eprintln!(
                        "muse shell: {command} answered after a read in {}ms",
                        started.elapsed().as_millis()
                    );
                    write(&serde_json::json!({ "id": id.clone(), "reply": reply }));
                    gtk4::glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => gtk4::glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    write(&serde_json::json!({
                        "id": id.clone(),
                        "reply": { "ok": false, "error": "the app's read died" }
                    }));
                    gtk4::glib::ControlFlow::Break
                }
            });
        }
    }
}

/// The ticket a page's command was asked under, read without any page state:
/// every answer the app writes has to land on the page's own request.
fn ticket(raw: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|request| request.get("id").cloned())
        .unwrap_or_else(|| serde_json::json!(0))
}

fn write(line: &serde_json::Value) {
    WRITER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(stdin) = slot.as_mut() else {
            return;
        };
        if let Err(error) = writeln!(stdin, "{line}").and_then(|()| stdin.flush()) {
            eprintln!("muse: cannot answer the shell window: {error}");
        }
    });
}

/// The window closed. If it ever answered a command, it was the app's surface
/// and the app ends with it; if it never did, it was a window that could not
/// come up, and the native one takes its place.
fn window_gone() {
    if !ANSWERED.load(Ordering::Relaxed) {
        eprintln!("muse: the shell window never answered; showing the native window");
        FALLEN_BACK.store(true, Ordering::Relaxed);
        FALLBACK.with(|slot| {
            if let Some(fallback) = slot.borrow().as_ref() {
                fallback();
            }
        });
        return;
    }
    eprintln!("muse: the shell window closed");
    QUIT.with(|slot| {
        if let Some(quit) = slot.borrow().as_ref() {
            quit();
        }
    });
}

/// The seat's library path without the entries that belong to this process's
/// own WebKit: the window loads the system GTK 3 / WebKitGTK 4.1, and an entry
/// naming a 6.0 bundle would give it the other generation.
fn clean_library_path(path: &str) -> Option<String> {
    let kept: Vec<&str> = path
        .split(':')
        .filter(|entry| !entry.is_empty())
        .filter(|entry| {
            !entry.contains("webkitgtk-6.0")
                && !entry.contains("muse-appimage")
                && !entry.contains("muse-toolbox")
        })
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join(":"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PUSHED, WRITER, candidates, clean_library_path, install, push_drawer_boot, serve, write,
    };
    use crate::pages::HistoryRow;
    use crate::webui::{WebRoutes, stub_routes};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::Ordering;

    #[test]
    fn the_window_is_looked_for_beside_the_app_and_in_its_own_project() {
        let candidates = candidates(std::path::Path::new("/repo/target/debug/muse"));
        assert_eq!(
            candidates[0],
            std::path::Path::new("/repo/target/debug/muse-window")
        );
        assert!(
            candidates
                .iter()
                .any(|path| path.ends_with("apps/muse-window/target/debug/muse-window")),
            "the dev checkout's own build is a candidate: {candidates:?}"
        );
    }

    #[test]
    fn the_windows_library_path_drops_this_process_own_webkit() {
        let inherited = "/run/user/1002/muse-appimage/usr/lib:/home/me/.local/share/muse-toolbox/gtk4/usr/lib:/usr/lib";
        assert_eq!(clean_library_path(inherited).as_deref(), Some("/usr/lib"));
        assert_eq!(clean_library_path("/tmp/.webkitgtk-6.0-umfl"), None);
        assert_eq!(
            clean_library_path("/usr/lib:/opt/webkit").as_deref(),
            Some("/usr/lib:/opt/webkit")
        );
    }

    /// A window's stdin that keeps what the app writes, standing in for the
    /// spawned pipe: the protocol is one JSON line per message either way.
    struct Recorder(Rc<RefCell<Vec<u8>>>);

    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn window_stdin() -> Rc<RefCell<Vec<u8>>> {
        let written = Rc::new(RefCell::new(Vec::new()));
        WRITER.with(|slot| *slot.borrow_mut() = Some(Box::new(Recorder(written.clone()))));
        written
    }

    fn lines(written: &Rc<RefCell<Vec<u8>>>) -> Vec<serde_json::Value> {
        String::from_utf8(written.borrow().clone())
            .expect("the app writes UTF-8")
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).expect("one message per line")
            })
            .collect()
    }

    /// The drawer's History is the panel's listing: the stub reads one row, so a
    /// payload's `history` is a read rather than an empty list.
    fn routes_with_history() -> WebRoutes {
        WebRoutes {
            history: Rc::new(|| {
                Ok(vec![HistoryRow {
                    id: "s-1".to_owned(),
                    text: "A calmer inbox".to_owned(),
                    current: true,
                }])
            }),
            ..stub_routes()
        }
    }

    /// `PUSHED` is the process's, not a test's: the two push tests take turns.
    fn boot_turn() -> std::sync::MutexGuard<'static, ()> {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A page's command crosses the pipe as the app's own answer, on the page's
    /// ticket: the same dispatcher and routes a native press reaches, with no
    /// WebView and no display in the way.
    #[test]
    fn a_page_command_is_answered_on_the_windows_own_pipe() {
        let written = window_stdin();
        install(None, routes_with_history());

        serve(r#"{"id":7,"cmd":"shell.snapshot"}"#);

        let answers = lines(&written);
        assert_eq!(answers.len(), 1, "one answer per command: {answers:?}");
        assert_eq!(answers[0]["id"], 7, "the page's ticket comes back");
        assert_eq!(answers[0]["reply"]["ok"], true);
        assert_eq!(
            answers[0]["reply"]["pages"].as_array().map(Vec::len),
            Some(crate::pages::PAGES.len())
        );
        assert_eq!(answers[0]["reply"]["history"][0]["text"], "A calmer inbox");
        assert_eq!(answers[0]["reply"]["history"][0]["current"], true);
    }

    /// A command answered before the pages are installed is answered in words,
    /// not with a silence the page would wait on.
    #[test]
    fn a_command_before_the_pages_are_installed_is_refused_on_the_pipe() {
        let written = window_stdin();

        serve(r#"{"id":3,"cmd":"chat.snapshot"}"#);

        let answers = lines(&written);
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0]["id"], 3);
        assert_eq!(answers[0]["reply"]["ok"], false);
        assert_eq!(
            answers[0]["reply"]["error"],
            "the app has no page state yet"
        );
    }

    /// Control lines are the same protocol: one JSON object, on its own line,
    /// with no ticket a reply could be mistaken for.
    #[test]
    fn a_control_line_is_one_json_object_on_its_own_line() {
        let written = window_stdin();

        write(&serde_json::json!({ "raise": true }));

        assert_eq!(lines(&written), vec![serde_json::json!({ "raise": true })]);
    }

    /// The drawer's one push: the page list, the name slot and the History rows
    /// the first paint could not carry, sent once and never asked for again.
    #[test]
    fn the_drawer_is_pushed_the_boot_it_missed_exactly_once() {
        let _turn = boot_turn();
        PUSHED.store(false, Ordering::Relaxed);
        let written = window_stdin();
        install(None, routes_with_history());

        push_drawer_boot();

        let pushed = lines(&written);
        assert_eq!(pushed.len(), 1, "one push, not a retry loop: {pushed:?}");
        assert!(
            pushed[0].get("id").is_none(),
            "a push answers no ticket: {pushed:?}"
        );
        let boot = &pushed[0]["boot"];
        assert_eq!(boot["page"], "shell");
        assert_eq!(
            boot["pages"].as_array().map(Vec::len),
            Some(crate::pages::PAGES.len())
        );
        assert!(
            boot["account"].is_string(),
            "the drawer's name slot travels: {boot}"
        );
        assert_eq!(boot["history"][0]["text"], "A calmer inbox");

        // The name arrives once, and so does the push: a later render of the
        // chat panel writes nothing.
        push_drawer_boot();
        assert_eq!(lines(&written).len(), 1, "the push is not repeated");
        PUSHED.store(false, Ordering::Relaxed);
    }

    /// A read that lands before the window is spawned is not a lost push: the
    /// launch's own boot request is asked after it and carries the name.
    #[test]
    fn a_name_read_before_the_window_starts_is_not_a_push() {
        let _turn = boot_turn();
        PUSHED.store(false, Ordering::Relaxed);
        WRITER.with(|slot| *slot.borrow_mut() = None);
        install(None, routes_with_history());

        push_drawer_boot();

        assert!(
            !PUSHED.load(Ordering::Relaxed),
            "nothing was written to, so nothing is marked as pushed"
        );
        PUSHED.store(false, Ordering::Relaxed);
    }
}
