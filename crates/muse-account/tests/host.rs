use muse_account::{AccountClient, BridgeConfig, Decision, ErrorCode};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let dir = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "muse-account-host-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn client(&self, python: &str, script: PathBuf) -> AccountClient {
        AccountClient::new(BridgeConfig {
            python: PathBuf::from(python),
            script,
            config_dir: self.0.join("config"),
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One request, one reply, on a socket that stands in for the host.
fn serve_once(socket: &Path, reply: &'static str) -> std::thread::JoinHandle<String> {
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut writer = stream;
        writer.write_all(reply.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
        line
    })
}

#[test]
fn a_host_envelope_is_served_without_starting_python() {
    let fixture = Fixture::new();
    let socket = fixture.0.join("host.sock");
    let server = serve_once(
        &socket,
        r#"{"ok":true,"data":{"identity_name":"Lotus","vm_id":"vm-9"}}"#,
    );
    // The python path cannot exist, so a passing result proves the socket was
    // used rather than the one-shot bridge.
    let client = fixture
        .client("/nonexistent/python", fixture.0.join("bridge.py"))
        .use_host_socket(socket);
    let identity = client.identity().unwrap();
    assert_eq!(identity.identity_name, "Lotus");
    assert_eq!(identity.vm_id, "vm-9");
    let line = server.join().unwrap();
    assert!(line.contains(r#""op":"identity""#), "{line}");
    assert!(line.ends_with('\n'), "one request per line");
}

#[test]
fn a_host_error_keeps_its_structured_code() {
    let fixture = Fixture::new();
    let socket = fixture.0.join("host.sock");
    let server = serve_once(
        &socket,
        r#"{"ok":false,"error":{"code":"auth_required","uncertain":false}}"#,
    );
    let client = fixture
        .client("/nonexistent/python", fixture.0.join("bridge.py"))
        .use_host_socket(socket);
    let error = client.identity().unwrap_err();
    assert_eq!(error.code, ErrorCode::AuthRequired);
    assert!(!error.uncertain);
    server.join().unwrap();
}

#[test]
fn a_missing_host_falls_back_to_the_one_shot_bridge() {
    // A stale install has no host script: requests must still be answered.
    let fixture = Fixture::new();
    let script = fixture.0.join("bridge.py");
    fs::write(
        &script,
        "import json, sys\nprint(json.dumps({'ok': True, 'data': [{'message_id': 'm', \
         'seq': 1, 'role': 'user', 'text': 'hi'}]}))\n",
    )
    .unwrap();
    let rows = fixture
        .client("/usr/bin/python3", script)
        .history(None, 20)
        .unwrap();
    assert_eq!(rows[0].message_id, "m");
}

const STUB_HOST: &str = r#"
import os, socket, sys

def fnv(data):
    v = 0xCBF29CE484222325
    for b in data:
        v ^= b
        v = (v * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return v

config_dir = sys.argv[sys.argv.index("--config-dir") + 1]
with open(os.path.join(config_dir, "stub-boot.log"), "a") as fh:
    fh.write("boot argv=%r\n" % (sys.argv,))
# The lifetime test pins the socket by writing this file; the derived path
# below stays the fallback for anything else that runs this stub.
pinned = os.path.join(config_dir, "host-socket-path")
if os.path.exists(pinned):
    with open(pinned) as fh:
        path = fh.read().strip()
else:
    root = os.environ.get("XDG_RUNTIME_DIR") or os.path.join(os.environ.get("HOME", ""), ".cache")
    path = os.path.join(root, "unofficial-muse-for-linux",
                        "account-%016x.sock" % fnv(os.fsencode(config_dir)))
os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    probe.connect(path)
    sys.exit(0)
except OSError:
    try:
        os.unlink(path)
    except OSError:
        pass
finally:
    probe.close()
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    listener.bind(path)
except OSError:
    import traceback
    with open(os.path.join(config_dir, "stub-error.log"), "a") as fh:
        fh.write("bind failed for %r (len %d):\n" % (path, len(path)))
        traceback.print_exc(file=fh)
    raise
listener.listen(8)
listener.settimeout(0.5)
with open(os.path.join(config_dir, "serve-starts.log"), "a") as fh:
    fh.write("serve start\n")
while os.path.exists(config_dir):
    try:
        conn, _ = listener.accept()
    except socket.timeout:
        continue
    except OSError:
        break
    try:
        data = b""
        while not data.endswith(b"\n"):
            chunk = conn.recv(65536)
            if not chunk:
                break
            data += chunk
        conn.sendall(b'{"ok":true,"data":{"identity_name":"Lotus","vm_id":"vm-9"}}\n')
    except OSError:
        pass
    finally:
        conn.close()
"#;

#[test]
fn a_host_spawned_by_a_short_lived_job_thread_survives_it() {
    // Regression (Nov 10, from Patrick's op-timing.log): start_host set
    // PR_SET_PDEATHSIG, which fires when the creating *thread* dies -- and
    // account jobs run on worker threads that exit with the job. Four
    // hosts started within twelve seconds, each serving one to three ops:
    // every host was killed by the completion of the job that spawned it,
    // so no held connection (and no watch) ever lived. The first call here
    // is made from a thread that dies immediately; the second must be
    // served by the SAME host.
    let fixture = Fixture::new();
    let config_dir = fixture.0.join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let script = fixture.0.join("muse_account_bridge.py");
    fs::write(&script, "# stub: only the sibling host script matters\n").unwrap();
    fs::write(fixture.0.join("muse_account_host.py"), STUB_HOST).unwrap();
    // The socket is pinned, not derived from a mutated XDG_RUNTIME_DIR:
    // `std::env::set_var` writes process-global state that every other test
    // thread reads (each account client copies the environment into every
    // child it spawns), so a parallel workspace run failed once in whichever
    // test happened to be reading at that instant -- the failure moved with
    // timing, not with any one test's logic. Pinning leaves this process's
    // environment untouched: the client gets the path through
    // `use_host_socket`, and the stub host reads the same path from a file in
    // its config dir. The derived path is still covered by
    // `the_socket_name_matches_the_published_fnv_1a_64_vectors`.
    //
    // The path stays SHORT: AF_UNIX paths cap at ~108 bytes, and nesting it
    // inside the fixture named after the pid pushed the socket past the limit
    // on CI (TMPDIR=/home/runner/work/_temp) and again on a host whose TMPDIR
    // is $HOME/.cache/muse-tmp once the pid reached six digits, so the stub's
    // bind failed ("AF_UNIX path too long") and the client fell back to the
    // one-shot bridge (ProtocolError).
    let socket = fixture.0.join("host.sock");
    fs::write(
        config_dir.join("host-socket-path"),
        socket.as_os_str().as_encoded_bytes(),
    )
    .unwrap();

    let first = fixture
        .client("/usr/bin/python3", script.clone())
        .use_host_socket(socket.clone());
    let first_outcome = std::thread::spawn(move || first.identity()).join().unwrap();
    if let Err(error) = &first_outcome {
        let boot = fs::read_to_string(config_dir.join("stub-boot.log"))
            .unwrap_or_else(|_| "<no stub boot log>".to_string());
        let starts = fs::read_to_string(config_dir.join("serve-starts.log"))
            .unwrap_or_else(|_| "<no serve starts>".to_string());
        let stub_error = fs::read_to_string(config_dir.join("stub-error.log"))
            .unwrap_or_else(|_| "<no stub error>".to_string());
        panic!(
            "first call failed: {error:?}\nstub boot: {boot}\nserve starts: {starts}\nstub error: {stub_error}"
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(700));
    let second = fixture
        .client("/usr/bin/python3", script)
        .use_host_socket(socket);
    second.identity().unwrap();

    let starts = fs::read_to_string(config_dir.join("serve-starts.log")).unwrap();
    assert_eq!(
        starts.lines().count(),
        1,
        "one host must serve both calls: {starts}"
    );
}

#[test]
fn the_socket_name_matches_the_published_fnv_1a_64_vectors() {
    // The Python host derives the same name from the same vectors; this is the
    // cross-language contract, not an implementation detail.
    assert_eq!(muse_account::fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(muse_account::fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(muse_account::fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    let path = muse_account::host_socket(Path::new("/home/x/.config/account"));
    assert_eq!(
        path,
        muse_account::host_socket(Path::new("/home/x/.config/account"))
    );
    assert!(path.to_string_lossy().ends_with(".sock"));
}

/// A host that takes the first request it ever sees and then closes the
/// connection without answering -- the ambiguous failure: the request may
/// already have been dispatched service-side. Every later request is
/// answered with an identity envelope, so a client that wrongly replays
/// the first request is both served and caught (the request log grows).
const DROP_FIRST_HOST: &str = r#"
import os, socket, sys

config_dir = sys.argv[sys.argv.index("--config-dir") + 1]
with open(os.path.join(config_dir, "host-socket-path")) as fh:
    path = fh.read().strip()
log = os.path.join(config_dir, "requests.log")
try:
    os.unlink(path)
except OSError:
    pass
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(path)
listener.listen(8)
listener.settimeout(0.5)
while os.path.exists(config_dir):
    try:
        conn, _ = listener.accept()
    except socket.timeout:
        continue
    except OSError:
        break
    try:
        data = b""
        while not data.endswith(b"\n"):
            chunk = conn.recv(65536)
            if not chunk:
                break
            data += chunk
        if data.endswith(b"\n"):
            with open(log, "a") as fh:
                fh.write(data.decode("utf-8", "replace"))
            with open(log) as fh:
                count = len(fh.read().splitlines())
            if count > 1:
                conn.sendall(b'{"ok":true,"data":{"identity_name":"Lotus","vm_id":"vm-9"}}\n')
            # count == 1: close without replying (the finally does it).
    except OSError:
        pass
    finally:
        conn.close()
"#;

struct StubHost(std::process::Child);

impl Drop for StubHost {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Write the drop-first stub where `start_host` looks for it (beside the
/// bridge script), pre-spawn one instance, and wait until it accepts, so
/// the client's first dispatch lands on it rather than on a spawn.
fn drop_first_fixture(fixture: &Fixture) -> (PathBuf, StubHost) {
    let config_dir = fixture.0.join("config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(fixture.0.join("muse_account_host.py"), DROP_FIRST_HOST).unwrap();
    let socket = fixture.0.join("host.sock");
    fs::write(
        config_dir.join("host-socket-path"),
        socket.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    let child = std::process::Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(fixture.0.join("muse_account_host.py"))
        .arg("--config-dir")
        .arg(&config_dir)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "stub host never listened"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    (socket, StubHost(child))
}

fn request_log(fixture: &Fixture) -> String {
    fs::read_to_string(fixture.0.join("config").join("requests.log")).unwrap_or_default()
}

/// One never-replay op under test: its wire name and the call that
/// submits it, with the answer discarded to a common shape.
type NeverReplayCall = (
    &'static str,
    fn(&AccountClient) -> Result<(), muse_account::AccountError>,
);

#[test]
fn never_replay_ops_are_not_redispatched_after_an_ambiguous_failure() {
    // Regression: host_exchange re-dispatched any op submitted with
    // sending=false on a fresh host after the first host dropped the
    // connection -- including ops whose effect must happen exactly once,
    // so an op that had already executed ran twice. Each op below must
    // surface the failure with exactly one request on the wire.
    let calls: [NeverReplayCall; 11] = [
        ("start_session", |c| c.start_session().map(|_| ())),
        ("tasks_run", |c| c.tasks_run("job-1").map(|_| ())),
        ("goal_create", |c| {
            c.goal_create("Title", "Summary", "Description", "🎯")
                .map(|_| ())
        }),
        ("goal_add_subgoal", |c| {
            c.goal_add_subgoal("goal-1", "Title", "Summary", "Description", "🎯")
                .map(|_| ())
        }),
        ("idea_execute", |c| c.idea_execute("card-1").map(|_| ())),
        ("attach_probe", |c| c.attach_probe("thread-1").map(|_| ())),
        ("message_react", |c| c.message_react("m1", "❤").map(|_| ())),
        ("decide", |c| {
            c.decide("approval-1", Decision::AllowOnce).map(|_| ())
        }),
        ("cancel", |c| c.cancel().map(|_| ())),
        ("space_share", |c| {
            c.space_share("space-1", "cloudflare", None).map(|_| ())
        }),
        ("idea_engagement", |c| {
            c.idea_engagement("idea-1").map(|_| ())
        }),
    ];
    for (op, call) in calls {
        let fixture = Fixture::new();
        let (socket, _host) = drop_first_fixture(&fixture);
        // python3 is real so a replay through start_host could actually
        // happen (and be logged) if the op lost its protection.
        let client = fixture
            .client("/usr/bin/python3", fixture.0.join("bridge.py"))
            .use_host_socket(socket);
        let error = call(&client).expect_err(&format!(
            "{op}: a dropped first answer must surface, never be replayed"
        ));
        assert_eq!(error.code, ErrorCode::BridgeFailure, "{op}");
        assert!(
            error.uncertain,
            "{op}: the effect may already have happened"
        );
        let log = request_log(&fixture);
        assert_eq!(
            log.lines().count(),
            1,
            "{op} must be dispatched exactly once, log: {log}"
        );
        assert!(log.contains(&format!(r#""op":"{op}""#)), "{op}: {log}");
    }
}

#[test]
fn a_read_still_retries_on_a_fresh_host_after_a_dropped_connection() {
    // The other half of the contract: replay-safe ops keep the retry --
    // the resilience is deliberate for them. The stub drops the first
    // request and answers the replay, so the read succeeds with exactly
    // two requests on the wire.
    let fixture = Fixture::new();
    let (socket, _host) = drop_first_fixture(&fixture);
    let client = fixture
        .client("/usr/bin/python3", fixture.0.join("bridge.py"))
        .use_host_socket(socket);
    let identity = client
        .identity()
        .expect("a read is replay-safe and must be retried");
    assert_eq!(identity.identity_name, "Lotus");
    let log = request_log(&fixture);
    assert_eq!(
        log.lines().count(),
        2,
        "exactly one replay of the read: {log}"
    );
    assert!(log.lines().all(|line| line.contains(r#""op":"identity""#)));
}

#[test]
fn the_pid_file_sits_beside_the_socket() {
    // The Python host publishes its pid at socket_path + ".pid"; the client
    // derives the same path to terminate a wedged host.
    let socket = Path::new("/run/user/1000/unofficial-muse-for-linux/account-ab12.sock");
    assert_eq!(
        muse_account::host_pid_path(socket),
        Path::new("/run/user/1000/unofficial-muse-for-linux/account-ab12.sock.pid")
    );
}
