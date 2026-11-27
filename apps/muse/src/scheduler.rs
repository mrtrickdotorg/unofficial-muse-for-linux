//! The app's run notifier.
//!
//! The service schedules and runs its own jobs. The pinned client publishes no
//! route that starts one -- its 258-entry route table carries `tasks.list`,
//! `tasks.runs`, `tasks.delete` and `tasks.subscribe`, and nothing else -- so a
//! client cannot fire a schedule, and pretending to would only produce a failed
//! request per due instant. What a desktop client can honestly do is tell the
//! reader about the runs the service makes: one read of the account's run list,
//! one ledger of the run ids already announced, one notification per newly
//! observed terminal run. That is the whole design.
//!
//! The read happens every minute, and every fifteen seconds while some run is
//! `running`, so a result is announced promptly without a poll per second. The
//! ledger (bounded, beside the account link) is seeded silently from the
//! account's existing history on the first launch: the app announces what
//! happens from then on, never what already happened. Two instances, or a
//! restart, therefore post each run once.
//!
//! The same read keeps the Tasks and Upcoming pages current, which is what the
//! listing read has always done.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Weak;
use std::time::Duration;

use muse_account::{Schedule, TaskRun, TasksSnapshot};

use crate::app::Chat;

/// How often the tick wakes. It only decides whether a read is due.
const TICK: Duration = Duration::from_secs(5);
/// The idle cadence: a run's result is not urgent to the second.
const READ_EVERY_SECS: i64 = 60;
/// The cadence while a run is in flight, so its result arrives promptly.
const RUNNING_READ_SECS: i64 = 15;
/// How many announced run ids the ledger keeps. The service's own listing
/// carries 25 runs, so this is history, not a working set.
const LEDGER_MAX: usize = 200;
/// A run the service still calls `running` this long after it came due is
/// reported once with the status it last had.
const STUCK_SECS: i64 = 30 * 60;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// The schedule's own name for its runs: the title it carries, or the id when
/// the service left the title empty.
fn schedule_name(schedules: &[Schedule], job_id: &str) -> String {
    schedules
        .iter()
        .find(|schedule| schedule.id == job_id)
        .map(|schedule| schedule.title.trim())
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| job_id.to_owned())
}

fn status_word(run: &TaskRun) -> &str {
    if run.status.trim().is_empty() {
        "finished"
    } else {
        run.status.trim()
    }
}

/// A run the service has not finished with yet.
fn in_flight(run: &TaskRun) -> bool {
    run.status.eq_ignore_ascii_case("running")
}

/// One line of at most `max` characters: the service's own words with their
/// line breaks folded into spaces, and an ellipsis where it had to be cut.
fn one_line(text: &str, max: usize) -> String {
    let mut joined = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(word);
    }
    match joined.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", joined[..cut].trim_end()),
        None => joined,
    }
}

/// The notification's own words for a run the service recorded.
fn notification_text(name: &str, run: &TaskRun) -> (String, String) {
    let summary = format!("Scheduled run: {name}");
    let detail = run
        .result_summary
        .as_deref()
        .or(run.error_text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let body = match detail {
        Some(detail) => format!("{} — {}", status_word(run), one_line(detail, 180)),
        None => format!("{} — the service recorded this run.", status_word(run)),
    };
    (summary, body)
}

/// The one sentence for a run that is still going long after it came due: the
/// last status the service gave, and where the reader can look.
fn deadline_text(name: &str, run: &TaskRun, now: i64) -> (String, String) {
    let summary = format!("Scheduled run: {name}");
    let minutes = (now - run.scheduled_for_utc.unwrap_or(now)).max(0) / 60;
    let body = format!(
        "Still {} {minutes} minutes after it came due; open Tasks for the run's own history.",
        status_word(run)
    );
    (summary, body)
}

/// The app's own config folder: the account link's parent, so a test home
/// scopes the notifier's ledger exactly as it scopes the link.
fn app_dir() -> Option<PathBuf> {
    crate::session::account_dir().and_then(|account| account.parent().map(Path::to_path_buf))
}

/// The announced run ids, written beside the account folder so a restart, or a
/// second instance taking over, does not announce them again.
fn ledger_path() -> Option<PathBuf> {
    app_dir().map(|dir| dir.join("notified-runs.json"))
}

/// The ledger as the file carries it. `None` when there is no file yet, which
/// is what makes the next read a seed rather than news; a file that is not a
/// list of ids is no memory at all, never an error the notifier has to handle.
fn load_ledger_at(path: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Vec<String>>(&text).ok()
}

/// Atomic, like the window's own writes: a temporary file beside it, then a
/// rename, so a reader never sees half a ledger. Best effort -- losing it costs
/// at most one repeated notification, never a lost run.
fn save_ledger_at(path: &Path, ids: &[String]) {
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string(ids) else {
        return;
    };
    let temporary = path.with_extension("json.part");
    if std::fs::write(&temporary, text).is_err() {
        return;
    }
    let _ = std::fs::rename(&temporary, path);
}

fn load_ledger() -> Option<Vec<String>> {
    ledger_path().and_then(|path| load_ledger_at(&path))
}

fn save_ledger(ids: &[String]) {
    if let Some(path) = ledger_path() {
        save_ledger_at(&path, ids);
    }
}

/// What one read says about the account's runs.
struct Observed {
    /// The notifications to post, in the listing's own order.
    notifications: Vec<(String, String)>,
    /// The ledger after this read.
    ledger: Vec<String>,
    /// Whether some run is still in flight, which tightens the next cadence.
    running: bool,
}

/// The decision for one read, as a function of the ledger and the listing.
///
/// The first read of a seat (`seeded == false`) is the account's history, not
/// news: its ids are recorded and nothing is posted. After that, a run is news
/// exactly once, and only when it has finished -- a run still going stays out
/// of the ledger, so its result is announced when it lands.
fn observe(ledger: &[String], seeded: bool, runs: &[TaskRun], schedules: &[Schedule]) -> Observed {
    let running = runs.iter().any(in_flight);
    if !seeded {
        let ids: Vec<String> = runs
            .iter()
            .filter(|run| !in_flight(run))
            .take(LEDGER_MAX)
            .map(|run| run.run_id.clone())
            .collect();
        return Observed {
            notifications: Vec::new(),
            ledger: ids,
            running,
        };
    }
    let mut notifications = Vec::new();
    let mut fresh: Vec<String> = Vec::new();
    for run in runs {
        if run.run_id.is_empty() || in_flight(run) || ledger.contains(&run.run_id) {
            continue;
        }
        if fresh.contains(&run.run_id) {
            continue;
        }
        notifications.push(notification_text(
            &schedule_name(schedules, &run.job_id),
            run,
        ));
        fresh.push(run.run_id.clone());
    }
    fresh.extend(ledger.iter().cloned());
    fresh.truncate(LEDGER_MAX);
    Observed {
        notifications,
        ledger: fresh,
        running,
    }
}

#[derive(Default)]
struct State {
    /// Whether the timer has been installed for this process.
    started: bool,
    /// Whether a read is on the wire.
    busy: bool,
    /// When the last read was taken, seed or not.
    read_at: i64,
    /// The run ids already announced, newest first.
    ledger: Vec<String>,
    /// Whether the ledger came from a file: a seat with no ledger seeds first.
    seeded: bool,
    /// Whether the last read saw a run in flight.
    running: bool,
    /// Long-running ids already reported in this process, so the one sentence
    /// about a stuck run is said once.
    stuck: HashSet<String>,
    /// Whether the current failed streak has been logged.
    logged_failure: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// What a read leaves behind for the caller to act on.
struct Settled {
    notifications: Vec<(String, String)>,
    /// The one line to log, when there is something worth saying once.
    line: Option<String>,
    /// Whether the ledger changed, and so has to be written.
    persist: bool,
}

/// Fold one read's answer into the state. A failed read posts nothing and says
/// one line per failed streak; a successful one announces each run it has not
/// announced before.
fn settle(state: &mut State, answer: Result<&TasksSnapshot, &str>, now: i64) -> Settled {
    state.busy = false;
    state.read_at = now;
    let snapshot = match answer {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let line = if state.logged_failure {
                None
            } else {
                state.logged_failure = true;
                Some(format!("the run list could not be read: {error}"))
            };
            return Settled {
                notifications: Vec::new(),
                line,
                persist: false,
            };
        }
    };
    state.logged_failure = false;
    let seeded = state.seeded;
    let before = state.ledger.clone();
    let observed = observe(&state.ledger, seeded, &snapshot.runs, &snapshot.schedules);
    state.ledger = observed.ledger;
    state.running = observed.running;
    state.seeded = true;
    let persist = !seeded || state.ledger != before;
    let fresh = observed.notifications.len();
    if !seeded {
        return Settled {
            notifications: Vec::new(),
            line: Some(format!(
                "seeded {} run id(s) from the account's existing history; nothing was announced",
                state.ledger.len()
            )),
            persist,
        };
    }
    let mut notifications = observed.notifications;
    for run in &snapshot.runs {
        let Some(due) = run.scheduled_for_utc else {
            continue;
        };
        if !in_flight(run) || now - due < STUCK_SECS || state.stuck.contains(&run.run_id) {
            continue;
        }
        state.stuck.insert(run.run_id.clone());
        notifications.push(deadline_text(
            &schedule_name(&snapshot.schedules, &run.job_id),
            run,
            now,
        ));
    }
    // Only ids still in flight are worth remembering: a run that finished is
    // reported by its result, not by this sentence.
    let live: HashSet<&str> = snapshot
        .runs
        .iter()
        .filter(|run| in_flight(run))
        .map(|run| run.run_id.as_str())
        .collect();
    state.stuck.retain(|id| live.contains(id.as_str()));
    Settled {
        notifications,
        // One line per read, so a log answers both halves of the question:
        // the poll happened, and it found nothing new.
        line: Some(format!("read {} run(s), {fresh} new", snapshot.runs.len())),
        persist,
    }
}

/// Start the timer once, for the life of the process, and install the click
/// route the notification needs.
pub(crate) fn start(chat: Weak<Chat>) {
    let already = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let already = state.started;
        state.started = true;
        already
    });
    if already {
        return;
    }
    crate::notify::set_click_handler(|| {
        // A run lives on the Tasks page: raise the window and paint that page,
        // which is the same route a press in the rail takes.
        eprintln!("muse notifier: a run's notification was clicked; opening Tasks");
        crate::web_shell::raise();
        crate::web_shell::show_page(crate::webui::tasks_boot());
    });
    // The ledger is read here, once. A file that exists means this seat has
    // announced before, so the first read is news; a file that does not means
    // the first read is the account's history, and seeds the ledger silently.
    let ledger = load_ledger();
    let seeded = ledger.is_some();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.seeded = seeded;
        state.ledger = ledger.unwrap_or_default();
    });
    eprintln!(
        "muse notifier: reading the account's runs every {READ_EVERY_SECS}s ({RUNNING_READ_SECS}s while a run is in flight); {} run(s) already announced",
        STATE.with(|state| state.borrow().ledger.len())
    );
    gtk4::glib::timeout_add_local(TICK, move || tick(&chat));
}

/// The tick decides only whether a read is due: a busy read is left to finish,
/// and the cadence follows what the last read saw.
fn tick(chat: &Weak<Chat>) -> gtk4::glib::ControlFlow {
    let Some(chat) = chat.upgrade() else {
        return gtk4::glib::ControlFlow::Break;
    };
    // Nothing is read before the account link exists.
    if chat.client_handle().is_none() {
        return gtk4::glib::ControlFlow::Continue;
    }
    let now = now_secs();
    let read_now = STATE.with(|state| {
        let state = state.borrow();
        if state.busy {
            return false;
        }
        let cadence = if state.running {
            RUNNING_READ_SECS
        } else {
            READ_EVERY_SECS
        };
        now - state.read_at >= cadence
    });
    if read_now {
        read(&chat);
    }
    gtk4::glib::ControlFlow::Continue
}

/// Read the account's run list on a worker, then announce what is new.
fn read(chat: &Chat) {
    let Some(client) = chat.client_handle() else {
        return;
    };
    STATE.with(|state| state.borrow_mut().busy = true);
    let context = gtk4::glib::MainContext::default();
    std::thread::spawn(move || {
        let answer = client.tasks().map_err(|error| error.to_string());
        context.invoke(move || {
            if let Ok(snapshot) = &answer {
                publish(snapshot);
            }
            let settled = STATE.with(|state| {
                settle(
                    &mut state.borrow_mut(),
                    answer.as_ref().map_err(String::as_str),
                    now_secs(),
                )
            });
            if let Some(line) = settled.line {
                eprintln!("muse notifier: {line}");
            }
            if settled.persist {
                let ledger = STATE.with(|state| state.borrow().ledger.clone());
                save_ledger(&ledger);
            }
            for (summary, body) in settled.notifications {
                crate::notify::notify(&summary, &body);
            }
        });
    });
}

/// Put a fresh listing in the app's caches: the Tasks rows, the Upcoming page,
/// and an open Tasks page, so a run shows in its history without the reader
/// touching anything.
fn publish(snapshot: &TasksSnapshot) {
    let held = crate::pages::held_tasks_from(snapshot);
    crate::pages::tasks_cache(held);
    crate::webui::upcoming_changed();
    crate::web_shell::page_changed("tasks");
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_account::{AgentActivity, Schedule, TaskRun};

    fn schedule(id: &str, title: &str) -> Schedule {
        Schedule {
            id: id.to_owned(),
            title: title.to_owned(),
            enabled: true,
            timezone: Some("America/Detroit".to_owned()),
            next_run_at_utc: None,
        }
    }

    fn run(run_id: &str, job: &str, status: &str) -> TaskRun {
        TaskRun {
            run_id: run_id.to_owned(),
            job_id: job.to_owned(),
            status: status.to_owned(),
            result_summary: None,
            error_text: None,
            scheduled_for_utc: None,
        }
    }

    fn snapshot(schedules: Vec<Schedule>, runs: Vec<TaskRun>) -> TasksSnapshot {
        TasksSnapshot {
            schedules,
            runs,
            agents: Vec::<AgentActivity>::new(),
        }
    }

    /// A run the service just finished is announced once: the same listing
    /// read again announces nothing, because the ledger now carries its id.
    #[test]
    fn a_new_terminal_run_notifies_once() {
        let listing = snapshot(
            vec![schedule("guard", "Extraction guard")],
            vec![run("run-1", "guard", "succeeded")],
        );
        let mut state = State {
            seeded: true,
            ledger: vec!["run-0".to_owned()],
            ..State::default()
        };
        let first = settle(&mut state, Ok(&listing), 1_000);
        assert_eq!(first.notifications.len(), 1);
        assert_eq!(first.notifications[0].0, "Scheduled run: Extraction guard");
        assert!(first.persist);
        let second = settle(&mut state, Ok(&listing), 1_060);
        assert!(second.notifications.is_empty(), "a run is announced once");
        assert!(!second.persist, "the second read changes no ledger");
    }

    /// A run already in the ledger stays quiet, even when it is the newest
    /// thing in the listing: a restart does not replay it.
    #[test]
    fn an_already_seen_run_does_not_notify() {
        let listing = snapshot(
            Vec::new(),
            vec![
                run("run-1", "guard", "failed"),
                run("run-2", "guard", "succeeded"),
            ],
        );
        let seeded = observe(
            &["run-1".to_owned()],
            true,
            &listing.runs,
            &listing.schedules,
        );
        let announced: Vec<&str> = seeded
            .notifications
            .iter()
            .map(|(summary, _)| summary.as_str())
            .collect();
        assert_eq!(announced, vec!["Scheduled run: guard"]);
        assert_eq!(seeded.ledger.first().map(String::as_str), Some("run-2"));

        let quiet = observe(
            &["run-1".to_owned(), "run-2".to_owned()],
            true,
            &listing.runs,
            &listing.schedules,
        );
        assert!(quiet.notifications.is_empty());
        assert_eq!(
            quiet.ledger.len(),
            2,
            "the ledger does not grow on a repeat"
        );
    }

    /// The first read of a seat is the account's history: its finished runs are
    /// recorded as already known and none of them is posted. A run still in
    /// flight is left out, so its result is announced when it lands.
    #[test]
    fn the_first_read_seeds_without_replaying() {
        let listing = snapshot(
            vec![schedule("guard", "Guard")],
            vec![
                run("run-3", "guard", "running"),
                run("run-2", "guard", "succeeded"),
                run("run-1", "guard", "failed"),
            ],
        );
        let mut state = State::default();
        let seeded = settle(&mut state, Ok(&listing), 2_000);
        assert!(seeded.notifications.is_empty(), "history is not news");
        assert!(seeded.persist, "the seed is written");
        assert_eq!(state.ledger.len(), 2);
        assert!(!state.ledger.contains(&"run-3".to_owned()));
        assert!(state.running);

        // The in-flight run finishing on the next read is news, once.
        let finished = snapshot(
            vec![schedule("guard", "Guard")],
            vec![run("run-3", "guard", "succeeded")],
        );
        let next = settle(&mut state, Ok(&finished), 2_060);
        assert_eq!(next.notifications.len(), 1);
        assert!(!state.running);
    }

    /// A read that failed says nothing on the desktop, and says so in the log
    /// once rather than on every tick of an outage.
    #[test]
    fn a_failed_read_posts_nothing() {
        let mut state = State {
            seeded: true,
            ledger: vec!["run-1".to_owned()],
            busy: true,
            ..State::default()
        };
        let first = settle(&mut state, Err("the gateway is down"), 3_000);
        assert!(first.notifications.is_empty());
        assert!(!first.persist);
        assert!(first.line.is_some_and(|line| line.contains("gateway")));
        assert!(!state.busy, "a failed read releases the slot");
        assert_eq!(state.ledger, vec!["run-1".to_owned()]);
        let second = settle(&mut state, Err("the gateway is down"), 3_060);
        assert!(second.notifications.is_empty());
        assert!(second.line.is_none(), "the outage is logged once");

        // A good read after the outage resets the line for the next one, and
        // says only what it read.
        let listing = snapshot(Vec::new(), Vec::new());
        let good = settle(&mut state, Ok(&listing), 3_120);
        assert!(!state.logged_failure);
        assert!(good.line.is_some_and(|line| line == "read 0 run(s), 0 new"));
    }

    /// A run the service still calls `running` long after it came due is said
    /// once, with the status it last had, and its result still gets its own
    /// notification when it lands.
    #[test]
    fn a_stuck_run_is_reported_once_and_its_result_still_lands() {
        let mut stuck = run("run-9", "guard", "running");
        stuck.scheduled_for_utc = Some(4_000);
        let listing = snapshot(vec![schedule("guard", "Guard")], vec![stuck.clone()]);
        let mut state = State {
            seeded: true,
            ..State::default()
        };
        let late = settle(&mut state, Ok(&listing), 4_000 + STUCK_SECS + 1);
        assert_eq!(late.notifications.len(), 1);
        assert!(late.notifications[0].1.contains("Still running"));
        let again = settle(&mut state, Ok(&listing), 4_000 + STUCK_SECS + 60);
        assert!(again.notifications.is_empty(), "said once");

        let mut done = stuck;
        done.status = "succeeded".to_owned();
        let finished = snapshot(vec![schedule("guard", "Guard")], vec![done]);
        let result = settle(&mut state, Ok(&finished), 4_000 + STUCK_SECS + 120);
        assert_eq!(result.notifications.len(), 1, "the result is still news");
        assert!(result.notifications[0].1.contains("succeeded"));
    }

    /// The ledger keeps the newest ids and never grows past its bound.
    #[test]
    fn the_ledger_stays_bounded() {
        let runs: Vec<TaskRun> = (0..LEDGER_MAX + 50)
            .map(|index| run(&format!("run-{index}"), "guard", "succeeded"))
            .collect();
        let observed = observe(&[], true, &runs, &[]);
        assert_eq!(observed.ledger.len(), LEDGER_MAX);
        assert_eq!(observed.ledger[0], "run-0");
        let carried = observe(&observed.ledger, true, &runs[..1], &[]);
        assert!(carried.notifications.is_empty());
        assert_eq!(carried.ledger.len(), LEDGER_MAX);
    }

    /// A schedule the listing names gives its title to the notification; one it
    /// does not name falls back to the job id rather than inventing a title.
    #[test]
    fn the_notification_names_the_schedule_it_can() {
        assert_eq!(schedule_name(&[schedule("a", "  ")][..], "a"), "a");
        assert_eq!(schedule_name(&[schedule("a", "Drain")][..], "a"), "Drain");
        assert_eq!(schedule_name(&[][..], "missing"), "missing");
    }

    /// The ledger round-trips through its file, and a file that is not a list of
    /// ids is treated as no ledger at all rather than as an error.
    #[test]
    fn the_ledger_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("muse-ledger-{}", std::process::id()));
        let path = dir.join("notified-runs.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_ledger_at(&path).is_none(), "no file is no ledger");
        save_ledger_at(&path, &["run-1".to_owned(), "run-2".to_owned()]);
        assert_eq!(
            load_ledger_at(&path),
            Some(vec!["run-1".to_owned(), "run-2".to_owned()])
        );
        std::fs::write(&path, "{\"not\":\"a list\"}").unwrap();
        assert!(load_ledger_at(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
