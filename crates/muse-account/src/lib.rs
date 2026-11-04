//! Opt-in account integration via a separately installed community Python bridge.
//! Constructors are inert; methods block and belong on a worker thread.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fmt,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_REQUEST: usize = 1024 * 1024;
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(180);

#[cfg(test)]
mod process_tests;

#[derive(Clone, Debug)]
pub struct BridgeConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    pub config_dir: PathBuf,
}
#[derive(Clone, Debug)]
pub struct AccountClient {
    config: BridgeConfig,
    active: Option<Arc<AtomicBool>>,
    /// Overrides the derived host socket (tests and embedding).
    host: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub identity_name: String,
    pub vm_id: String,
    pub threads: Vec<Thread>,
}
/// Identity only: `AccountSnapshot` also carries every conversation, and the
/// service can spend seconds answering that list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountIdentity {
    pub identity_name: String,
    pub vm_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub session_id: String,
    pub title: String,
    pub archived: bool,
    pub updated_at: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub message_id: String,
    pub seq: u64,
    pub role: String,
    pub text: String,
    pub timestamp: Option<String>,
    pub status: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksSnapshot {
    pub schedules: Vec<Schedule>,
    pub runs: Vec<TaskRun>,
}
/// Bounded live listen: what the agent is doing right now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveStatus {
    pub events: Vec<LiveEvent>,
}
/// One live event, allowlisted by the bridge. Threaded replies never arrive
/// here, so `text` is a progress fragment, never the answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveEvent {
    pub event: String,
    #[serde(default)]
    pub seq: Option<u64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub title: String,
    pub enabled: bool,
    pub timezone: Option<String>,
    pub next_run_at_utc: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRun {
    pub run_id: String,
    pub job_id: String,
    pub status: String,
    pub result_summary: Option<String>,
    pub error_text: Option<String>,
    pub scheduled_for_utc: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchReceipt {
    pub sent: bool,
    pub confirmed: bool,
    pub message_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    AuthRequired,
    AccountChanged,
    VmUnavailable,
    Timeout,
    InvalidRequest,
    InsecureConfig,
    BridgeUnavailable,
    BridgeFailure,
    ProtocolError,
    ResponseTooLarge,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountError {
    pub code: ErrorCode,
    pub uncertain: bool,
    /// Local-only context, such as the interpreter that could not be started.
    /// The bridge never sends this, so a returned `bridge_unavailable` keeps it
    /// empty while a failed spawn fills it: the two causes stay distinguishable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
impl AccountError {
    fn new(code: ErrorCode, uncertain: bool) -> Self {
        Self {
            code,
            uncertain,
            detail: None,
        }
    }
    fn with_detail(code: ErrorCode, uncertain: bool, detail: impl Into<String>) -> Self {
        Self {
            code,
            uncertain,
            detail: Some(detail.into()),
        }
    }
}
impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.code {
            ErrorCode::AuthRequired => "Muse authentication is missing or expired; sign in again.",
            ErrorCode::AccountChanged => {
                "The saved Muse session now belongs to a different account; reconnect to continue."
            }
            ErrorCode::VmUnavailable => "The Muse account VM is unavailable.",
            ErrorCode::Timeout => "The account bridge exceeded its time limit.",
            ErrorCode::InvalidRequest => "The account request or bridge configuration is invalid.",
            ErrorCode::InsecureConfig => "Account configuration permissions are unsafe.",
            ErrorCode::BridgeUnavailable => {
                "The selected community account bridge is not installed or executable."
            }
            ErrorCode::BridgeFailure => "The community account gateway request failed.",
            ErrorCode::ProtocolError => "The account bridge returned an invalid response.",
            ErrorCode::ResponseTooLarge => "The account bridge response exceeded its size limit.",
        })?;
        if self.uncertain {
            f.write_str(" Delivery is uncertain; do not automatically resend.")?;
        }
        if let Some(detail) = &self.detail {
            f.write_str(&format!(" ({detail})"))?;
        }
        Ok(())
    }
}
impl std::error::Error for AccountError {}

#[derive(Deserialize)]
struct Envelope<T> {
    ok: bool,
    data: Option<T>,
    error: Option<AccountError>,
}

impl AccountClient {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            config,
            active: None,
            host: None,
        }
    }
    /// Bound this clone's subprocesses to a session lease. False cancels locally;
    /// an already dispatched remote action cannot be recalled and is uncertain.
    pub fn cancellable(&self, active: Arc<AtomicBool>) -> Self {
        Self {
            config: self.config.clone(),
            active: Some(active),
            host: self.host.clone(),
        }
    }
    /// Point this client at a specific host socket instead of the derived one.
    #[doc(hidden)]
    pub fn use_host_socket(&self, path: PathBuf) -> Self {
        Self {
            config: self.config.clone(),
            active: self.active.clone(),
            host: Some(path),
        }
    }
    pub fn connect(&self) -> Result<AccountSnapshot, AccountError> {
        self.request(serde_json::json!({"op": "connect"}), false)
    }
    /// Cheap account verification, for a preflight which must not pay for the
    /// conversation list. `connect` remains the way to load that list.
    pub fn identity(&self) -> Result<AccountIdentity, AccountError> {
        self.request(serde_json::json!({"op": "identity"}), false)
    }
    pub fn history(
        &self,
        thread: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HistoryMessage>, AccountError> {
        validate_thread(thread)?;
        if !(1..=200).contains(&limit) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.request(
            serde_json::json!({"op": "history", "thread": thread, "limit": limit}),
            false,
        )
    }
    /// Wait until the newest message changes, then return the window.
    ///
    /// The bridge holds one connection for the wait, so a recheck costs ~0.1s
    /// instead of a fresh Noise handshake per tick.
    pub fn watch(
        &self,
        thread: Option<&str>,
        limit: usize,
        timeout: Duration,
    ) -> Result<Vec<HistoryMessage>, AccountError> {
        self.watch_after(thread, limit, None, timeout)
    }
    /// Wait for anything newer than `after`, then return the window.
    ///
    /// A caller that already knows the newest message it has seen passes it
    /// here: the bridge then answers on its first read instead of waiting for
    /// the *next* change, so nothing that arrived while the request was being
    /// set up is missed.
    pub fn watch_after(
        &self,
        thread: Option<&str>,
        limit: usize,
        after: Option<&str>,
        timeout: Duration,
    ) -> Result<Vec<HistoryMessage>, AccountError> {
        validate_thread(thread)?;
        if !(1..=200).contains(&limit) {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let millis = u64::try_from(timeout.as_millis())
            .ok()
            .filter(|millis| (1..=60_000).contains(millis))
            .ok_or_else(|| AccountError::new(ErrorCode::InvalidRequest, false))?;
        let mut request = serde_json::json!({
            "op": "watch",
            "thread": thread,
            "limit": limit,
            "timeout": millis,
        });
        if let Some(after) = after {
            if after.is_empty() || after.len() > 256 {
                return Err(AccountError::new(ErrorCode::InvalidRequest, false));
            }
            request["after"] = after.into();
        }
        self.request(request, false)
    }
    pub fn tasks(&self) -> Result<TasksSnapshot, AccountError> {
        self.request(serde_json::json!({"op": "tasks"}), false)
    }
    /// Listen briefly for live agent/task status.
    ///
    /// Not a reply read: the pinned client records that threaded replies never
    /// arrive as live events, so the transcript keeps polling `history` and this
    /// feeds only the status line. Bounded to 30 s so a status read cannot hold
    /// the shared control connection for a whole minute.
    pub fn activity(&self, timeout: Duration) -> Result<LiveStatus, AccountError> {
        let millis = u64::try_from(timeout.as_millis())
            .ok()
            .filter(|millis| (1..=30_000).contains(millis))
            .ok_or_else(|| AccountError::new(ErrorCode::InvalidRequest, false))?;
        self.request(
            serde_json::json!({"op": "activity", "timeout": millis}),
            false,
        )
    }
    /// Dispatch once; an unconfirmed receipt or uncertain error must not be
    /// automatically retried. Confirmation proves only prompt persistence.
    pub fn send(
        &self,
        thread: Option<&str>,
        message: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        self.send_checked(thread, message, None, None)
    }
    /// Dispatch only after proving, in the same request, that the saved session
    /// is still `vm_id` for `identity_name`. One account check instead of a
    /// whole `connect`, which also lists every conversation.
    pub fn send_verified(
        &self,
        thread: Option<&str>,
        message: &str,
        vm_id: &str,
        identity_name: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        self.send_checked(thread, message, None, Some((vm_id, identity_name)))
    }
    pub fn send_from_device_verified(
        &self,
        thread: Option<&str>,
        message: &str,
        device_id: &str,
        vm_id: &str,
        identity_name: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        self.send_checked(
            thread,
            message,
            Some(device_id),
            Some((vm_id, identity_name)),
        )
    }
    /// Explicit registration attribution; never changes saved account settings.
    pub fn send_from_device(
        &self,
        thread: Option<&str>,
        message: &str,
        device_id: &str,
    ) -> Result<DispatchReceipt, AccountError> {
        if device_id.is_empty()
            || device_id.len() > 128
            || !device_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        self.send_checked(thread, message, Some(device_id), None)
    }
    fn send_checked(
        &self,
        thread: Option<&str>,
        message: &str,
        device_id: Option<&str>,
        expect: Option<(&str, &str)>,
    ) -> Result<DispatchReceipt, AccountError> {
        validate_thread(thread)?;
        if message.trim().is_empty() || message.len() > 512 * 1024 {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        if let Some(device_id) = device_id
            && (device_id.is_empty()
                || device_id.len() > 128
                || !device_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let mut request = serde_json::json!({"op": "send", "thread": thread, "message": message});
        if let Some(device_id) = device_id {
            request["device_id"] = device_id.into();
        }
        if let Some((vm_id, identity_name)) = expect {
            if vm_id.is_empty()
                || vm_id.len() > 256
                || identity_name.is_empty()
                || identity_name.len() > 256
            {
                return Err(AccountError::new(ErrorCode::InvalidRequest, false));
            }
            request["expect_vm_id"] = vm_id.into();
            request["expect_identity_name"] = identity_name.into();
        }
        let receipt: DispatchReceipt = self.request(request, true)?;
        if receipt.confirmed
            && (!receipt.sent || receipt.message_id.as_deref().is_none_or(str::is_empty))
        {
            return Err(AccountError::new(ErrorCode::ProtocolError, true));
        }
        Ok(receipt)
    }
    fn request<T: DeserializeOwned>(
        &self,
        mut request: serde_json::Value,
        sending: bool,
    ) -> Result<T, AccountError> {
        if !self.config.python.is_absolute()
            || !self.config.script.is_absolute()
            || !self.config.config_dir.is_absolute()
        {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        let config_dir = self
            .config
            .config_dir
            .to_str()
            .ok_or_else(|| AccountError::new(ErrorCode::InvalidRequest, false))?;
        request["config_dir"] = serde_json::Value::String(config_dir.to_owned());
        let bytes = serde_json::to_vec(&request)
            .map_err(|_| AccountError::new(ErrorCode::InvalidRequest, false))?;
        if bytes.len() > MAX_REQUEST {
            return Err(AccountError::new(ErrorCode::InvalidRequest, false));
        }
        // A live host answers in ~0.1s on a connection it already holds. When
        // there is none (or it cannot start), the one-shot bridge below is
        // unchanged: a missing host must never block an account operation.
        let output = match self.host_exchange(&bytes, sending)? {
            Some(output) => output,
            None => {
                // Only the one-shot path needs the interpreter and script; a
                // host that is already running answers without either.
                self.require_bridge()?;
                run_bridge_active(
                    &self.config,
                    &bytes,
                    DEADLINE,
                    sending,
                    self.active.as_deref(),
                )?
            }
        };
        let envelope: Envelope<T> = serde_json::from_slice(&output)
            .map_err(|_| AccountError::new(ErrorCode::ProtocolError, sending))?;
        if envelope.ok {
            if envelope.error.is_some() {
                return Err(AccountError::new(ErrorCode::ProtocolError, sending));
            }
            envelope
                .data
                .ok_or_else(|| AccountError::new(ErrorCode::ProtocolError, sending))
        } else {
            Err(envelope
                .error
                .unwrap_or_else(|| AccountError::new(ErrorCode::ProtocolError, sending)))
        }
    }
    /// One request line to the host, one reply line back.
    ///
    /// `Ok(None)` means no host is available, so the caller keeps its original
    /// one-shot path. A failure after the request was written is an error, never
    /// a retry: the host may already have dispatched it.
    fn host_exchange(
        &self,
        request: &[u8],
        sending: bool,
    ) -> Result<Option<Vec<u8>>, AccountError> {
        let socket = self
            .host
            .clone()
            .unwrap_or_else(|| host_socket(&self.config.config_dir));
        if let Ok(stream) = UnixStream::connect(&socket) {
            match self.exchange_on(stream, request, sending) {
                Ok(line) => return Ok(Some(line)),
                // A socket left behind by a host that has just exited: a read is
                // idempotent and can be retried on a fresh host, a send cannot
                // be replayed because it may already have been written.
                Err(_) if !sending => return self.start_and_exchange(&socket, request),
                Err(error) => return Err(error),
            }
        }
        self.start_and_exchange(&socket, request)
    }
    fn start_and_exchange(
        &self,
        socket: &Path,
        request: &[u8],
    ) -> Result<Option<Vec<u8>>, AccountError> {
        match self.start_host().and_then(|()| wait_for_socket(socket)) {
            Some(stream) => self.exchange_on(stream, request, false).map(Some),
            None => Ok(None),
        }
    }
    fn exchange_on(
        &self,
        stream: UnixStream,
        request: &[u8],
        sending: bool,
    ) -> Result<Vec<u8>, AccountError> {
        let fail = |code: ErrorCode| AccountError::new(code, sending);
        let started = Instant::now();
        stream
            .set_write_timeout(Some(DEADLINE))
            .map_err(|_| fail(ErrorCode::BridgeFailure))?;
        // Short reads so a cancelled lease is noticed during a long wait.
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| fail(ErrorCode::BridgeFailure))?;
        let mut writer = stream
            .try_clone()
            .map_err(|_| fail(ErrorCode::BridgeFailure))?;
        if self
            .active
            .as_deref()
            .is_some_and(|active| !active.load(Ordering::SeqCst))
        {
            return Err(fail(ErrorCode::BridgeFailure));
        }
        writer
            .write_all(request)
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .map_err(|error| {
                AccountError::with_detail(
                    ErrorCode::BridgeFailure,
                    sending,
                    format!("host write: {error}"),
                )
            })?;
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            if self
                .active
                .as_deref()
                .is_some_and(|active| !active.load(Ordering::SeqCst))
            {
                return Err(fail(ErrorCode::BridgeFailure));
            }
            if started.elapsed() >= DEADLINE {
                return Err(fail(ErrorCode::Timeout));
            }
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => {
                    return Err(AccountError::with_detail(
                        ErrorCode::BridgeFailure,
                        sending,
                        "host closed the connection".to_owned(),
                    ));
                }
                Ok(_) if line.last() == Some(&b'\n') => {
                    line.pop();
                    return Ok(line);
                }
                Ok(_) => {
                    if line.len() > MAX_RESPONSE {
                        return Err(fail(ErrorCode::ResponseTooLarge));
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => {
                    return Err(AccountError::with_detail(
                        ErrorCode::BridgeFailure,
                        sending,
                        format!("host read: {error}"),
                    ));
                }
            }
        }
    }
    fn require_bridge(&self) -> Result<(), AccountError> {
        if !self.config.python.is_file() {
            return Err(AccountError::with_detail(
                ErrorCode::BridgeUnavailable,
                false,
                format!("interpreter not found: {}", self.config.python.display()),
            ));
        }
        if !self.config.script.is_file() {
            return Err(AccountError::with_detail(
                ErrorCode::BridgeUnavailable,
                false,
                format!("bridge script not found: {}", self.config.script.display()),
            ));
        }
        Ok(())
    }
    fn start_host(&self) -> Option<()> {
        // The host lives beside the bridge script; a stale install has none and
        // falls back instead of failing.
        let script = self.config.script.with_file_name("muse_account_host.py");
        if !script.is_file() {
            return None;
        }
        let mut command = Command::new(&self.config.python);
        bridge_environment(&mut command);
        // SAFETY: prctl is async-signal-safe and only gives the child a
        // parent-death signal of its own; nothing here touches parent state.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        command
            .arg("-I")
            .arg(&script)
            .arg("--config-dir")
            .arg(&self.config.config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .ok()?;
        // Dropped, never killed: the host exits when it is orphaned or idle.
        Some(())
    }
}

/// FNV-1a 64. The Python host derives its socket name the same way, so this
/// function is a cross-language contract rather than an internal detail.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut value: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        value ^= u64::from(*byte);
        value = value.wrapping_mul(0x0000_0100_0000_01b3);
    }
    value
}

pub fn host_socket(config_dir: &Path) -> PathBuf {
    let name = format!(
        "account-{:016x}.sock",
        fnv1a64(config_dir.as_os_str().as_encoded_bytes())
    );
    let root = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => match std::env::var_os("XDG_CACHE_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"),
        },
    };
    root.join("unofficial-muse-for-linux").join(name)
}

fn bridge_environment(command: &mut Command) {
    const KEYS: [&str; 21] = [
        "HOME",
        "PATH",
        "LANG",
        "LC_ALL",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ];
    command.env_clear();
    for key in KEYS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("MUSE_MCP_ENABLE_RAW", "0")
        .env("PYTHONDONTWRITEBYTECODE", "1");
}

fn wait_for_socket(path: &Path) -> Option<UnixStream> {
    let started = Instant::now();
    // First launch imports the bridge and opens two connections; give the
    // process a moment to bind before deciding it is absent.
    while started.elapsed() < Duration::from_secs(5) {
        if let Ok(stream) = UnixStream::connect(path) {
            return Some(stream);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

fn validate_thread(thread: Option<&str>) -> Result<(), AccountError> {
    if thread.is_some_and(|s| s.trim().is_empty() || s.len() > 4096 || s.contains('\0')) {
        return Err(AccountError::new(ErrorCode::InvalidRequest, false));
    }
    Ok(())
}

/// Kill the dedicated process group before reaping, including a descendant which
/// might otherwise keep stdout open. No wait/read can outlive the deadline.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // SAFETY: the child was started as leader of its own process group.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn nonblocking(fd: i32) -> std::io::Result<()> {
    // SAFETY: fcntl accesses a valid pipe descriptor owned by this call.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(test)]
fn run_bridge(
    config: &BridgeConfig,
    request: &[u8],
    timeout: Duration,
    sending: bool,
) -> Result<Vec<u8>, AccountError> {
    run_bridge_active(config, request, timeout, sending, None)
}
fn run_bridge_active(
    config: &BridgeConfig,
    request: &[u8],
    timeout: Duration,
    sending: bool,
    active: Option<&AtomicBool>,
) -> Result<Vec<u8>, AccountError> {
    if active.is_some_and(|a| !a.load(Ordering::SeqCst)) {
        return Err(AccountError::new(ErrorCode::InvalidRequest, false));
    }
    let mut command = Command::new(&config.python);
    bridge_environment(&mut command);
    let child = command
        .arg("-I")
        .arg(&config.script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| {
            AccountError::with_detail(
                ErrorCode::BridgeUnavailable,
                false,
                format!("could not start {}: {error}", config.python.display()),
            )
        })?;
    let mut guard = ChildGuard(child);
    let io_error = || AccountError::new(ErrorCode::BridgeFailure, sending);
    let stdin = guard.0.stdin.take().ok_or_else(io_error)?;
    let mut stdout = guard.0.stdout.take().ok_or_else(io_error)?;
    nonblocking(stdin.as_raw_fd()).map_err(|_| io_error())?;
    nonblocking(stdout.as_raw_fd()).map_err(|_| io_error())?;
    let started = Instant::now();
    let mut input = Some(stdin);
    let mut written = 0;
    let mut output = Vec::new();
    let mut eof = false;
    loop {
        if active.is_some_and(|a| !a.load(Ordering::SeqCst)) {
            return Err(AccountError::new(ErrorCode::BridgeFailure, sending));
        }
        if started.elapsed() >= timeout {
            return Err(AccountError::new(ErrorCode::Timeout, sending));
        }
        if let Some(pipe) = input.as_mut() {
            match pipe.write(&request[written..]) {
                Ok(0) => {
                    input = None;
                }
                Ok(n) => {
                    written += n;
                    if written == request.len() {
                        input = None;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    input = None;
                }
                Err(_) => return Err(io_error()),
            }
        }
        // Read only bounded chunks and return to timeout checks even for a flood.
        for _ in 0..16 {
            let mut chunk = [0; 8192];
            match stdout.read(&mut chunk) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    if output.len() + n > MAX_RESPONSE {
                        return Err(AccountError::new(ErrorCode::ResponseTooLarge, sending));
                    }
                    output.extend_from_slice(&chunk[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(io_error()),
            }
        }
        if let Some(status) = guard.0.try_wait().map_err(|_| io_error())?
            && eof
        {
            if !status.success() {
                return Err(io_error());
            }
            return Ok(output);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
