use muse_reminder_runner::{run_cycle, Notifier};
use muse_tasks::{Task, TaskStore};
use std::cell::RefCell;
use std::path::PathBuf;

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "muse-runner-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // The store refuses group/world-accessible directories.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Recorder {
    sent: RefCell<Vec<u64>>,
    fail_ids: Vec<u64>,
}

impl Notifier for Recorder {
    fn notify(&self, task: &Task) -> Result<(), &'static str> {
        if self.fail_ids.contains(&task.id) {
            return Err("test refusal");
        }
        self.sent.borrow_mut().push(task.id);
        Ok(())
    }
}

#[test]
fn a_cycle_claims_and_notifies_exactly_once_across_cycles() {
    let dir = tmpdir("once");
    let t = now();
    let (due_id, future_id) = {
        let mut store = TaskStore::open(&dir).unwrap();
        let due = store.create("due", "body", t - 30).unwrap();
        let future = store.create("future", "", t + 3600).unwrap();
        (due.id, future.id)
    };
    let recorder = Recorder {
        sent: RefCell::new(Vec::new()),
        fail_ids: Vec::new(),
    };
    let mut logs = Vec::new();
    let mut log = |line: String| logs.push(line);
    // Catch-up: the task went due before the runner ever started; the first
    // cycle still fires it — exactly once.
    assert_eq!(run_cycle(&dir, &recorder, t, &mut log).unwrap(), 1);
    assert_eq!(*recorder.sent.borrow(), vec![due_id]);
    assert_eq!(run_cycle(&dir, &recorder, t, &mut log).unwrap(), 0);
    assert_eq!(recorder.sent.borrow().len(), 1);
    let _ = future_id;
    let _ = logs;
}

#[test]
fn a_failed_notification_is_logged_by_id_and_retried_until_it_lands() {
    let dir = tmpdir("fail");
    let t = now();
    let due_id = {
        let mut store = TaskStore::open(&dir).unwrap();
        store.create("due", "secret body", t - 30).unwrap().id
    };
    let recorder = Recorder {
        sent: RefCell::new(Vec::new()),
        fail_ids: vec![due_id],
    };
    let mut logs = Vec::new();
    let claimed = run_cycle(&dir, &recorder, t, &mut |line| logs.push(line)).unwrap();
    assert_eq!(claimed, 1, "the claim happened even though delivery failed");
    assert!(recorder.sent.borrow().is_empty());
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains(&due_id.to_string()));
    assert!(
        !logs[0].contains("secret body"),
        "reminder text must never reach a log"
    );
    // The claim is released, so the reminder is still due: nothing spent a
    // reminder nobody saw. A second failing cycle tries again, and the store
    // still says it was never delivered.
    let mut logs2 = Vec::new();
    assert_eq!(
        run_cycle(&dir, &recorder, t, &mut |line| logs2.push(line)).unwrap(),
        1
    );
    assert_eq!(logs2.len(), 1);
    let task = TaskStore::open(&dir).unwrap().get(due_id).unwrap();
    assert!(!task.notified, "an undelivered reminder is not marked seen");

    // A notifier that works delivers it once, and from then on the store is
    // quiet: the retry did not turn into a duplicate.
    let working = Recorder {
        sent: RefCell::new(Vec::new()),
        fail_ids: Vec::new(),
    };
    assert_eq!(run_cycle(&dir, &working, t, &mut |_| {}).unwrap(), 1);
    assert_eq!(*working.sent.borrow(), vec![due_id]);
    assert_eq!(run_cycle(&dir, &working, t, &mut |_| {}).unwrap(), 0);
    assert_eq!(working.sent.borrow().len(), 1);
    assert!(TaskStore::open(&dir).unwrap().get(due_id).unwrap().notified);
}
