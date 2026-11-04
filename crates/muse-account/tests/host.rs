use muse_account::{AccountClient, BridgeConfig, ErrorCode};
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
