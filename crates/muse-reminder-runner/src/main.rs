//! muse-reminder-runner — the user-service half of local reminders.
//!
//! Usage: muse-reminder-runner [--dir <store-dir>] [--interval-secs <n>]
//!                             [--once]
//!
//! Without `--once` it cycles forever: claim what is due, notify, sleep.
//! It owns no state of its own — the store is the state — so stopping,
//! restarting or crashing it can never duplicate a reminder (claims are
//! durable and at-most-once) or lose one that has not been claimed yet
//! (it fires on the next cycle, however late: catch-up by construction).

use muse_reminder_runner::{default_store_dir, now_unix_secs, run_cycle, NotifySend};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut dir: Option<PathBuf> = None;
    let mut interval_secs: u64 = 20;
    let mut once = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--once" => once = true,
            "--dir" => match args.next() {
                Some(value) => dir = Some(PathBuf::from(value)),
                None => {
                    eprintln!("--dir needs a value");
                    return ExitCode::from(2);
                }
            },
            "--interval-secs" => match args.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(value) if value >= 5 => interval_secs = value,
                _ => {
                    eprintln!("--interval-secs needs a number of at least 5");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir.or_else(default_store_dir) else {
        eprintln!("no store directory: HOME is unset and --dir was not given");
        return ExitCode::from(2);
    };
    let notifier = NotifySend;
    loop {
        let mut log = |line: String| eprintln!("{line}");
        match run_cycle(&dir, &notifier, now_unix_secs(), &mut log) {
            Ok(claimed) => {
                if claimed > 0 {
                    eprintln!("delivered {claimed} due reminder(s)");
                }
            }
            // A broken store (corrupt file, vanished directory) is reported
            // per cycle, never fatal: the service stays up for the cycle
            // where it is fixed, and systemd's journal keeps the evidence.
            Err(error) => eprintln!("cycle failed: {error}"),
        }
        if once {
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(std::time::Duration::from_secs(interval_secs));
    }
}
