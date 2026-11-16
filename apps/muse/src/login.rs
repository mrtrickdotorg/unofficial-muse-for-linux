//! Browser sign-in for the rewrite: launches the isolated sign-in
//! browser (fresh private profile, never any installed browser's),
//! waits for the reader to authenticate, and saves the session cookie
//! into the session folder. Ported from the shipped client: the pump
//! loop, the cancellation discipline, and the error mapping are
//! unchanged, because the threat model is unchanged (a helper that
//! outlives cancellation must never publish credentials later).
//!
//! Why a browser instead of the Mac's attested onboarding: the Mac
//! completes `onboarding.pages` with platform attestation this OS
//! cannot produce. The goal is the same linked session the Mac gets;
//! only the mechanism is platform-specific.

use std::ffi::OsString;
use std::os::unix::io::AsRawFd;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Maps a helper error code to the reader-facing sentence. Raw helper
/// output is never shown: it can carry credential-adjacent detail.
pub fn login_error_message(code: &str) -> &'static str {
    match code {
        "invalid_request" => "Sign-in helper rejected the selected account directory.",
        "missing_browser" | "browser_not_found" | "browser_unavailable" => {
            "No supported Chromium-compatible browser was available."
        }
        "missing_dependency" => {
            "The selected Python could not run the isolated browser sign-in helper."
        }
        "dependency_missing" | "sdk_unavailable" => {
            "The selected Python is missing the community Muse account or browser dependency."
        }
        "browser_launch_failed" => "The isolated browser could not be started.",
        "unsafe_devtools" => "The isolated browser debugging connection could not be verified.",
        "devtools_error" => "The isolated browser connection failed before sign-in completed.",
        "timeout" | "login_timeout" => "Browser sign-in expired before authentication completed.",
        "unsafe_config" | "insecure_config" => {
            "Browser sign-in needs a folder that belongs to you and is private (owner-only, no symlinks). Pick a new empty session folder — the default already qualifies — or make that folder owner-only if it holds nothing else. The Muse CLI folder cannot be used for browser sign-in."
        }
        "cancelled" | "browser_closed" => {
            "Browser sign-in was cancelled or the isolated browser was closed."
        }
        "cleanup_failed" => {
            "Close the dedicated sign-in browser before trying again; browser cleanup could not be confirmed."
        }
        _ => {
            "The sign-in helper reported failure. Check the selected interpreter, isolated browser and account directory; raw output is hidden to protect credentials."
        }
    }
}

/// Cancellation shared with the worker: the flag stops the pump, the
/// pid lets the main thread signal the helper without waiting for it.
#[derive(Default)]
pub struct Cancellation {
    cancelled: AtomicBool,
    pid: Mutex<Option<u32>>,
}

impl Cancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let pid = match self.pid.try_lock() {
            Ok(pid) => pid,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            // Never wait for subprocess cleanup on GTK's main thread. The
            // worker sees the atomic flag and sends SIGTERM itself as needed.
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        if let Some(pid) = *pid {
            // The lock prevents reaping/PID reuse while this signal is sent.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

struct Guard<'a> {
    child: Child,
    cancellation: &'a Cancellation,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let mut pid = self
            .cancellation
            .pid
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pid.is_some() {
            // Give our helper time to close its separately grouped browser and
            // remove its private temporary profile before escalating.
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(6);
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
            *pid = None;
        }
    }
}

impl Guard<'_> {
    fn status(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let mut pid = self
            .cancellation
            .pid
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let status = self.child.try_wait()?;
        if status.is_some() {
            *pid = None;
        }
        Ok(status)
    }
}

fn safe_environment(
    values: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    const ALLOWED: &[&str] = &[
        "HOME",
        "USER",
        "PATH",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_DATA_DIRS",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ];
    values
        .into_iter()
        .filter(|(key, _)| key.to_str().is_some_and(|key| ALLOWED.contains(&key)))
        .collect()
}

fn nonblocking(fd: i32) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Run the isolated browser sign-in to completion: pump the request
/// in, read the bounded reply, and return Ok only when the helper
/// reports `ok`. Cancellation stops the pump and the helper; a
/// helper that outlives cancellation cannot publish later because
/// the cookie only lands via this function's Ok path.
pub fn run_sign_in(
    python: &Path,
    script: &Path,
    config_dir: &Path,
    cancellation: &Cancellation,
) -> Result<(), String> {
    let cancelled_message = || "Browser sign-in was cancelled.".to_string();
    if cancellation.is_cancelled() {
        return Err(cancelled_message());
    }
    if !python.is_absolute() || !script.is_file() || !config_dir.is_absolute() {
        return Err("The browser sign-in helper or selected configuration is unavailable.".into());
    }
    let child = Command::new(python)
        .env_clear()
        .envs(safe_environment(std::env::vars_os()))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg("-I")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Could not start the browser sign-in helper.".to_string())?;
    {
        let mut pid = cancellation.pid.lock().unwrap_or_else(|e| e.into_inner());
        *pid = Some(child.id());
        if cancellation.is_cancelled() {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
        }
    }
    let mut guard = Guard {
        child,
        cancellation,
    };
    let mut input = guard.child.stdin.take();
    let mut stdout = guard
        .child
        .stdout
        .take()
        .ok_or_else(|| "Sign-in response pipe is unavailable.".to_string())?;
    let request = serde_json::to_vec(&serde_json::json!({"config_dir":config_dir}))
        .map_err(|_| "The selected sign-in directory is invalid.".to_string())?;
    let io_error = || "The sign-in helper connection failed; raw output is hidden.".to_string();
    if let Some(input) = &input {
        nonblocking(input.as_raw_fd()).map_err(|_| io_error())?;
    }
    nonblocking(stdout.as_raw_fd()).map_err(|_| io_error())?;
    let mut written = 0;
    let mut output = Vec::new();
    let mut eof = false;
    let deadline = Instant::now() + Duration::from_secs(330);
    let mut cancel_deadline = None;
    let status = loop {
        if cancellation.is_cancelled() && cancel_deadline.is_none() {
            cancellation.cancel();
            cancel_deadline = Some(Instant::now() + Duration::from_secs(6));
        }
        if cancel_deadline.is_some_and(|limit| Instant::now() >= limit) {
            return Err(cancelled_message());
        }
        if Instant::now() >= deadline {
            return Err("Browser sign-in timed out. The helper was stopped.".into());
        }
        if let Some(pipe) = &mut input {
            use std::io::Write;
            match pipe.write(&request[written..]) {
                Ok(0) => input = None,
                Ok(n) => {
                    written += n;
                    if written == request.len() {
                        input = None;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(io_error()),
            }
        }
        for _ in 0..16 {
            use std::io::Read;
            let mut buffer = [0; 4096];
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    if output.len() + n > 65_536 {
                        return Err("Sign-in helper exceeded its safe response limit.".into());
                    }
                    output.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(io_error()),
            }
        }
        if let Some(status) = guard.status().map_err(|_| io_error())?
            && eof
        {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if cancellation.is_cancelled() {
        return Err(cancelled_message());
    }
    let result: serde_json::Value = serde_json::from_slice(&output)
        .map_err(|_| "Sign-in helper returned invalid data; raw output is hidden.".to_string())?;
    if status.success() && result.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }
    let code = result
        .get("error")
        .and_then(|error| {
            error
                .as_str()
                .or_else(|| error.get("code").and_then(serde_json::Value::as_str))
        })
        .unwrap_or("internal_error");
    Err(login_error_message(code).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn error_codes_map_to_credential_safe_sentences() {
        assert!(login_error_message("browser_launch_failed").contains("could not be started"));
        assert!(login_error_message("cancelled").contains("cancelled"));
        assert!(login_error_message("nope").contains("raw output is hidden"));
    }

    fn fixture_script(body: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .iter()
            .map(std::path::PathBuf::from)
            .find(|path| path.is_file())?;
        let root = std::env::temp_dir().join(format!(
            "muse-login-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).ok()?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).ok()?;
        std::fs::write(root.join("helper.py"), body).ok()?;
        Some((python, root.join("helper.py")))
    }

    #[test]
    fn helper_ok_means_linked() {
        let Some((python, script)) = fixture_script(
            "import sys,json\njson.load(sys.stdin)\nsys.stdout.write('{\"ok\": true}')\n",
        ) else {
            return;
        };
        let dir = script.parent().unwrap().to_owned();
        let done = run_sign_in(&python, &script, &dir, &Cancellation::default());
        std::fs::remove_dir_all(&dir).ok();
        assert!(done.is_ok());
    }

    #[test]
    fn helper_error_maps_through_the_safe_sentences() {
        let Some((python, script)) = fixture_script(
            "import sys,json\njson.load(sys.stdin)\nsys.stdout.write('{\"ok\": false, \"error\": \"browser_launch_failed\"}')\n",
        ) else {
            return;
        };
        let dir = script.parent().unwrap().to_owned();
        let done = run_sign_in(&python, &script, &dir, &Cancellation::default());
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            done.unwrap_err(),
            "The isolated browser could not be started."
        );
    }

    #[test]
    fn pre_cancelled_sign_in_never_spawns() {
        let cancel = Cancellation::default();
        cancel.cancel();
        let done = run_sign_in(
            std::path::Path::new("/usr/bin/python3"),
            std::path::Path::new("/nonexistent/helper.py"),
            std::path::Path::new("/tmp"),
            &cancel,
        );
        assert_eq!(done.unwrap_err(), "Browser sign-in was cancelled.");
    }
}
