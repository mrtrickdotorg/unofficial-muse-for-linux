//! Debug-only seam for the throwaway Tauri 2 spike.
//!
//! The spike runs in its own process on GTK3/WebKitGTK 4.1, while this app is
//! GTK4/WebKitGTK 6.0, so the two cannot share a process -- and the held chat
//! store is a thread-local in *this* process, filled by this window's own
//! paints. This module lets the spike ask this process the page's own
//! questions: one JSON line per connection, answered by
//! [`crate::webui::handle_command_with`] on the main loop, against the rows and
//! statuses this window has actually painted, with the same client and routes
//! the chat window's handler gets.
//!
//! Compiled only with `--features spike-cmd`, and inert unless `MUSE_SPIKE_CMD`
//! names a socket path. Any local process that can open that socket could
//! drive the page's command surface, sends and decisions included, so this must
//! never be part of a shipped build.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::rc::Rc;
use std::sync::mpsc::{TryRecvError, channel};
use std::time::{Duration, Instant};

use crate::webui::{Answer, WebRoutes, handle_command_with, reply_for};

/// How often the main loop looks at a worker's answer, the same cadence the
/// chat window's own reply path uses.
const ANSWER_POLL: Duration = Duration::from_millis(100);

type Answerer = Rc<dyn Fn(&str) -> (serde_json::Value, Answer, String)>;

thread_local! {
    static ANSWER: std::cell::RefCell<Option<Answerer>> =
        const { std::cell::RefCell::new(None) };
}

/// Install the seam on the app's main thread, with the client and routes the
/// chat window would be opened with.
pub(crate) fn install(path: &str, chat: &Rc<crate::app::Chat>) {
    let client = chat.client_handle();
    let routes: WebRoutes = chat.web_routes();
    ANSWER.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(move |raw: &str| {
            handle_command_with(raw, client.clone(), Some(&routes))
        }));
    });
    let path = path.to_owned();
    std::thread::spawn(move || listen(&path));
}

fn listen(path: &str) {
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("spike-cmd: cannot bind {path}: {error}");
            return;
        }
    };
    eprintln!("spike-cmd: answering on {path}");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // One command per connection, so a slow read cannot stall the next
        // request behind it.
        std::thread::spawn(move || serve(stream));
    }
}

fn serve(stream: UnixStream) {
    let mut line = String::new();
    let read = {
        let mut reader = BufReader::new(&stream);
        reader.read_line(&mut line)
    };
    if !matches!(read, Ok(count) if count > 0) {
        return;
    }
    let request = line.trim().to_owned();
    let started = Instant::now();
    let (sender, receiver) = channel::<(serde_json::Value, serde_json::Value)>();
    let request_for_main = request.clone();
    // The handler reads the held store, so it must run on this window's main
    // thread -- the thread the paints wrote to. The handler itself is looked
    // up there, because the store it reads is a thread-local of that thread.
    gtk4::glib::MainContext::default().invoke(move || {
        let Some(answerer) = ANSWER.with(|slot| slot.borrow().clone()) else {
            let _ = sender.send((
                serde_json::json!(0),
                serde_json::json!({ "ok": false, "error": "spike-cmd: no handler installed" }),
            ));
            return;
        };
        match answerer(&request_for_main) {
            (id, Answer::Now(reply), command) => {
                eprintln!(
                    "spike-cmd: {command} answered from the held state in {} ms",
                    started.elapsed().as_millis()
                );
                let _ = sender.send((id, reply));
            }
            (id, Answer::Later(pending), command) => {
                gtk4::glib::timeout_add_local(ANSWER_POLL, move || match pending.try_recv() {
                    Ok(fetched) => {
                        let reply = reply_for(fetched, &command, started);
                        eprintln!(
                            "spike-cmd: {command} answered after a read in {} ms",
                            started.elapsed().as_millis()
                        );
                        let _ = sender.send((id.clone(), reply));
                        gtk4::glib::ControlFlow::Break
                    }
                    Err(TryRecvError::Empty) => gtk4::glib::ControlFlow::Continue,
                    Err(TryRecvError::Disconnected) => {
                        let _ = sender.send((
                            serde_json::json!(0),
                            serde_json::json!({ "ok": false, "error": "spike-cmd: the read died" }),
                        ));
                        gtk4::glib::ControlFlow::Break
                    }
                });
            }
        }
    });
    if let Ok((id, reply)) = receiver.recv() {
        let _ = write_line(stream, &serde_json::json!({ "id": id, "reply": reply }));
    }
}

fn write_line(mut stream: UnixStream, body: &serde_json::Value) -> std::io::Result<()> {
    stream.write_all(body.to_string().as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}
