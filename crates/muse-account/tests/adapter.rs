use muse_account::{AccountClient, BridgeConfig};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new(script: &str) -> Self {
        let dir = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "muse-account-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("bridge.py"), script).unwrap();
        Self(dir)
    }
    fn client(&self) -> AccountClient {
        AccountClient::new(BridgeConfig {
            python: PathBuf::from("/usr/bin/python3"),
            script: self.0.join("bridge.py"),
            config_dir: self.0.join("config"),
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn history_preserves_selected_scope_ids_roles_timestamps_and_status() {
    let f = Fixture::new(
        r#"
import json, sys
r = json.load(sys.stdin)
assert r['op'] == 'history' and r['thread'] == ' exact-id ' and r['limit'] == 20
print(json.dumps({'ok':True,'data':[{'message_id':'msg', 'seq':9, 'role':'assistant',
    'text':'response','timestamp':'123','status':'complete'}]}))
"#,
    );
    let history = f.client().history(Some(" exact-id "), 20).unwrap();
    assert_eq!(history[0].message_id, "msg");
    assert_eq!(history[0].seq, 9);
    assert_eq!(history[0].role, "assistant");
    assert_eq!(history[0].timestamp.as_deref(), Some("123"));
    assert_eq!(history[0].status.as_deref(), Some("complete"));
    assert!(f.client().history(None, 0).is_err());
    assert!(f.client().history(None, 201).is_err());
    assert!(f.client().history(Some("  "), 1).is_err());
}

#[test]
fn activity_reads_live_status_without_claiming_the_reply() {
    use std::time::Duration;
    let f = Fixture::new(
        r#"
import json, sys
r = json.load(sys.stdin)
assert r['op'] == 'activity' and r['timeout'] == 8000
print(json.dumps({'ok': True, 'data': {'events': [
    {'event': 'agent.status', 'seq': 5, 'status': 'Is working', 'session_id': 's1'},
    {'event': 'task.status', 'seq': 6, 'status': 'completed'}]}}))
"#,
    );
    let live = f.client().activity(Duration::from_secs(8)).unwrap();
    assert_eq!(live.events.len(), 2);
    assert_eq!(live.events[0].event, "agent.status");
    assert_eq!(live.events[0].status.as_deref(), Some("Is working"));
    assert_eq!(live.events[0].session_id.as_deref(), Some("s1"));
    assert_eq!(live.events[1].seq, Some(6));
    assert!(live.events[1].text.is_none());
    // A status read is bounded: no zero-length and no minute-long listens.
    assert!(f.client().activity(Duration::ZERO).is_err());
    assert!(f.client().activity(Duration::from_secs(31)).is_err());
    assert!(f.client().activity(Duration::from_millis(30_001)).is_err());
}

#[test]
fn cancel_reports_the_service_answer_instead_of_assuming_a_stop() {
    let idle = Fixture::new(
        r#"
import json, sys
r = json.load(sys.stdin)
assert r['op'] == 'cancel'
assert 'message' not in r and 'thread' not in r
print(json.dumps({'ok': True, 'data': {'cancelled': False, 'message_id': None}}))
"#,
    );
    let answer = idle.client().cancel().unwrap();
    assert!(!answer.cancelled, "nothing was running, so nothing stopped");
    assert!(answer.message_id.is_none());

    let running = Fixture::new(
        r#"
import json, sys
r = json.load(sys.stdin)
assert r['op'] == 'cancel'
print(json.dumps({'ok': True, 'data': {'cancelled': True, 'message_id': 'm-1',
                                        'access_token': 'SECRET'}}))
"#,
    );
    let stopped = running.client().cancel().unwrap();
    assert!(stopped.cancelled);
    assert_eq!(stopped.message_id.as_deref(), Some("m-1"));
}

#[test]
fn cancelled_device_lease_never_dispatches_and_interrupts_inflight_bridge() {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };
    let f = Fixture::new("import sys,time\nsys.stdin.read()\ntime.sleep(30)\n");
    let active = Arc::new(AtomicBool::new(false));
    let client = f.client().cancellable(active.clone());
    assert!(
        client
            .send_from_device(None, "hello", "linux-test")
            .is_err()
    );
    active.store(true, Ordering::SeqCst);
    let start = Instant::now();
    let worker = std::thread::spawn(move || client.send_from_device(None, "hello", "linux-test"));
    std::thread::sleep(Duration::from_millis(80));
    active.store(false, Ordering::SeqCst);
    assert!(worker.join().unwrap().is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn explicit_device_send_preserves_device_and_rejects_malformed_ids() {
    let f = Fixture::new(
        r#"
import json, sys
r = json.load(sys.stdin)
assert r['device_id'] == 'linux-test-123' and r['thread'] == 'scope'
print(json.dumps({'ok':True,'data':{'sent':True,'confirmed':False,'message_id':None}}))
"#,
    );
    assert!(
        f.client()
            .send_from_device(Some("scope"), "hello", "linux-test-123")
            .unwrap()
            .sent
    );
    for bad in ["", " space ", "a/b", "ü"] {
        let error = f.client().send_from_device(None, "hello", bad).unwrap_err();
        assert_eq!(error.code, muse_account::ErrorCode::InvalidRequest);
        assert!(!error.uncertain);
    }
}

#[test]
fn tasks_parses_schedule_and_run_types() {
    let f = Fixture::new(
        r#"
import json, sys
assert json.load(sys.stdin)['op'] == 'tasks'
print(json.dumps({'ok':True,'data':{'schedules':[{'id':'job-1','title':'Daily','enabled':True,
 'timezone':'UTC','next_run_at_utc':123}],'runs':[{'run_id':'run-1','job_id':'job-1','status':'success',
 'result_summary':'Done','error_text':None,'scheduled_for_utc':120}]}}))
"#,
    );
    let result = f.client().tasks().unwrap();
    assert_eq!(result.schedules[0].id, "job-1");
    assert_eq!(result.schedules[0].next_run_at_utc, Some(123));
    assert_eq!(result.runs[0].status, "success");
    assert_eq!(result.runs[0].scheduled_for_utc, Some(120));
}

#[test]
fn listing_ops_parse_the_projected_rows() {
    // One fixture per surface: the request carries only the op, and the
    // projected listing keeps its recognized/skipped accounting intact.
    let ideas = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'ideas'
assert 'limit' not in request
print(json.dumps({'ok': True, 'data': {'total': 2, 'recognized': 1, 'skipped': 1, 'items': [
  {'id': 'card-1', 'title': 'Try a morning brief', 'body': 'A short brief.',
   'kind': 'habit', 'stamp': '2026-11-08'}]}}))
"#,
    );
    let page = ideas.client().ideas().unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.recognized, 1);
    assert_eq!(page.skipped, 1);
    assert_eq!(page.items[0].id.as_deref(), Some("card-1"));
    assert_eq!(page.items[0].title, "Try a morning brief");
    assert_eq!(page.items[0].body.as_deref(), Some("A short brief."));
    assert_eq!(page.items[0].kind.as_deref(), Some("habit"));
    assert_eq!(page.items[0].stamp.as_deref(), Some("2026-11-08"));

    let goals = Fixture::new(
        r#"
import json, sys
assert json.load(sys.stdin)['op'] == 'goals'
print(json.dumps({'ok': True, 'data': {'total': 1, 'recognized': 1, 'skipped': 0, 'items': [
  {'id': 'g1', 'title': 'Ship the Linux client', 'body': None,
   'kind': 'active', 'stamp': None}]}}))
"#,
    );
    let page = goals.client().goals().unwrap();
    assert_eq!(page.items[0].title, "Ship the Linux client");
    assert_eq!(page.items[0].body, None);
    assert_eq!(page.items[0].stamp, None);

    let feed = Fixture::new(
        r#"
import json, sys
assert json.load(sys.stdin)['op'] == 'feed'
print(json.dumps({'ok': True, 'data': {'total': 0, 'recognized': 0, 'skipped': 0,
 'items': []}}))
"#,
    );
    let page = feed.client().feed().unwrap();
    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
}

#[test]
fn artifacts_parses_one_projected_library_page() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'artifacts'
assert request['config_dir'].endswith('/config')
assert 'limit' not in request
print(json.dumps({'ok': True, 'data': {'total_count': 76, 'has_more': True, 'artifacts': [
  {'artifact_ref': 'file:workspace/your_files/a.docx', 'title': 'a.docx', 'kind': 'document',
   'format': 'docx', 'status': 'ready', 'placement': 'library',
   'path': '/workspace/your_files/a.docx', 'updated_at_ms': 1793915135823,
   'pinned': False, 'can_open': True, 'can_share': False}]}}))
"#,
    );
    let page = f.client().artifacts().unwrap();
    assert_eq!(page.total_count, 76);
    assert!(page.has_more);
    assert_eq!(page.artifacts.len(), 1);
    let row = &page.artifacts[0];
    assert_eq!(row.artifact_ref, "file:workspace/your_files/a.docx");
    assert_eq!(row.title, "a.docx");
    assert_eq!(row.format.as_deref(), Some("docx"));
    assert_eq!(row.path.as_deref(), Some("/workspace/your_files/a.docx"));
    assert_eq!(row.updated_at_ms, Some(1_793_915_135_823));
    assert!(!row.pinned);
    assert!(row.can_open);
    assert!(!row.can_share, "sharing stays the service's answer");
}

#[test]
fn permissions_parses_the_projected_active_grants() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'permissions'
assert request['config_dir'].endswith('/config')
print(json.dumps({'ok': True, 'data': {'grants': [
  {'permission_id': 'p-1', 'kind': 'egress', 'lifetime': 'always', 'mode': 'allow',
   'policy_subject': 'example.com', 'expires_at_ms': 1799999999000,
   'revocable': True, 'title': 'example.com'},
  {'permission_id': 'p-2', 'kind': 'connector', 'lifetime': None, 'mode': None,
   'policy_subject': None, 'expires_at_ms': None, 'revocable': False, 'title': ''}]}}))
"#,
    );
    let list = f.client().permissions().unwrap();
    assert_eq!(list.grants.len(), 2);
    let grant = &list.grants[0];
    assert_eq!(grant.permission_id, "p-1");
    assert_eq!(grant.policy_subject.as_deref(), Some("example.com"));
    assert_eq!(grant.expires_at_ms, Some(1_799_999_999_000));
    assert!(grant.revocable);
    assert!(!list.grants[1].revocable);
}

#[test]
fn revoke_sends_one_named_grant_and_reports_acceptance_only() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'revoke' and request['permission_id'] == 'p-1'
print(json.dumps({'ok': True, 'data': {'permission_id': 'p-1', 'accepted': True,
                                          'revoked': True}}))
"#,
    );
    let answer = f.client().revoke("p-1").unwrap();
    assert!(answer.accepted);
    assert_eq!(answer.revoked, Some(true));
    // A malformed id is refused before a child is ever spawned.
    assert!(f.client().revoke("").is_err());
    assert!(f.client().revoke("x".repeat(257).as_str()).is_err());
}

#[test]
fn delete_task_sends_one_named_schedule_and_reports_acceptance_only() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'delete_task' and request['job_id'] == 'job-1'
print(json.dumps({'ok': True, 'data': {'job_id': 'job-1', 'accepted': True,
                                          'deleted': True}}))
"#,
    );
    let answer = f.client().delete_task("job-1").unwrap();
    assert!(answer.accepted);
    assert_eq!(answer.deleted, Some(true));
    // A malformed id is refused before a child is ever spawned.
    assert!(f.client().delete_task("").is_err());
    assert!(f.client().delete_task("x".repeat(257).as_str()).is_err());
}

#[test]
fn an_unidentified_library_row_is_a_protocol_error_not_an_empty_list() {
    let f = Fixture::new(
        r#"
import json, sys
assert json.load(sys.stdin)['op'] == 'artifacts'
print(json.dumps({'ok': False, 'error': {'code': 'protocol_error', 'uncertain': False}}))
"#,
    );
    let error = f.client().artifacts().unwrap_err();
    assert_eq!(error.code, muse_account::ErrorCode::ProtocolError);
    assert!(!error.uncertain);
}

#[test]
fn send_does_not_treat_dispatched_as_confirmed_or_retry() {
    let f = Fixture::new(
        r#"
import json, sys, pathlib
assert len(sys.argv) == 1
r = json.load(sys.stdin)
assert r['op'] == 'send' and r['thread'] == 'thread-1' and r['message'] == 'exact prompt'
marker = pathlib.Path(__file__).with_suffix('.sent')
assert not marker.exists(), 'must not retry'
marker.touch()
print(json.dumps({'ok':True,'data':{'sent':True,'confirmed':False,'message_id':None}}))
"#,
    );
    let receipt = f.client().send(Some("thread-1"), "exact prompt").unwrap();
    assert!(receipt.sent);
    assert!(!receipt.confirmed);
    assert!(receipt.message_id.is_none());
    assert!(f.client().send(None, "  ").is_err());
}

#[test]
fn probe_is_read_only_by_default_and_prints_only_counts() {
    let f = Fixture::new(
        r#"
import json, sys
r = json.load(sys.stdin)
if r['op'] == 'connect':
 data = {'identity_name':'PRIVATE_NAME','vm_id':'PRIVATE_VM','threads':[]}
elif r['op'] == 'history':
 data = [{'message_id':'PRIVATE_ID','seq':1,'role':'user','text':'PRIVATE_MESSAGE','timestamp':None,'status':None}]
elif r['op'] == 'tasks':
 data = {'schedules':[],'runs':[]}
else:
 raise AssertionError('must not send')
print(json.dumps({'ok':True,'data':data}))
"#,
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_account-probe"))
        .args([
            "/usr/bin/python3",
            f.0.join("bridge.py").to_str().unwrap(),
            f.0.to_str().unwrap(),
        ])
        .env_remove("MUSE_ACCOUNT_PROBE_ALLOW_SEND")
        .env("MUSE_ACCOUNT_PROBE_SEND_TEXT", "must not implicitly send")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        text,
        "connected=true threads=0 history_messages=1 schedules=0 runs=0\n"
    );
    assert!(!text.contains("PRIVATE"));
}

#[test]
fn structured_auth_vm_and_timeout_errors_remain_typed_and_safe() {
    use muse_account::ErrorCode;
    for (wire, expected) in [
        ("auth_required", ErrorCode::AuthRequired),
        ("vm_unavailable", ErrorCode::VmUnavailable),
        ("timeout", ErrorCode::Timeout),
    ] {
        let f = Fixture::new(&format!(
            r#"
import json, sys
json.load(sys.stdin)
print(json.dumps({{'ok':False,'error':{{'code':'{wire}','uncertain':False,'message':'SECRET'}}}}))
"#
        ));
        let error = f.client().connect().unwrap_err();
        assert_eq!(error.code, expected);
        assert!(!error.uncertain);
        assert!(!format!("{error} {error:?}").contains("SECRET"));
    }
}

#[test]
fn malformed_output_and_failed_child_never_echo_stdout_or_stderr() {
    use muse_account::ErrorCode;
    for (script, expected) in [
        ("print('SECRET malformed JSON')", ErrorCode::ProtocolError),
        (
            "import sys\nsys.stderr.write('SECRET credential')\nsys.exit(1)",
            ErrorCode::BridgeFailure,
        ),
        (
            "print('{\"ok\":false,\"error\":{\"code\":\"SECRET\",\"uncertain\":false}}')",
            ErrorCode::ProtocolError,
        ),
        (
            "print('{\"ok\":true,\"data\":{\"identity_name\":true}}')",
            ErrorCode::ProtocolError,
        ),
    ] {
        let f = Fixture::new(script);
        let error = f.client().send(None, "prompt").unwrap_err();
        assert_eq!(error.code, expected);
        assert!(error.uncertain);
        assert!(!format!("{error} {error:?}").contains("SECRET"));
    }
}

#[test]
fn construction_is_inert_and_invalid_inputs_never_spawn_a_child() {
    use muse_account::ErrorCode;
    fn send_sync<T: Send + Sync>() {}
    send_sync::<AccountClient>();
    let f = Fixture::new("raise AssertionError('must not execute')");
    let mut config = BridgeConfig {
        python: PathBuf::from("/usr/bin/python3"),
        script: f.0.join("bridge.py"),
        config_dir: PathBuf::from("relative"),
    };
    let client = AccountClient::new(config.clone());
    let _clone = client.clone();
    assert_eq!(
        client.connect().unwrap_err().code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        client
            .send(None, &"x".repeat(512 * 1024 + 1))
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        client.history(Some("bad\0thread"), 1).unwrap_err().code,
        ErrorCode::InvalidRequest
    );
    config.config_dir = f.0.clone();
    config.python = f.0.join("missing-python");
    let error = AccountClient::new(config).connect().unwrap_err();
    assert_eq!(error.code, ErrorCode::BridgeUnavailable);
    assert!(!error.uncertain);
}

#[test]
fn inconsistent_confirmation_receipt_is_rejected_as_uncertain() {
    use muse_account::ErrorCode;
    let f = Fixture::new(
        "print('{\"ok\":true,\"data\":{\"sent\":true,\"confirmed\":true,\"message_id\":null}}')",
    );
    let error = f.client().send(None, "prompt").unwrap_err();
    assert_eq!(error.code, ErrorCode::ProtocolError);
    assert!(error.uncertain);
}

#[test]
fn bridge_receives_only_allowlisted_environment() {
    let f = Fixture::new(
        r#"
import json, os, sys
assert os.environ['MUSE_MCP_ENABLE_RAW'] == '0'
assert os.environ['PYTHONDONTWRITEBYTECODE'] == '1'
for forbidden in ('OPENAI_API_KEY','ANTHROPIC_API_KEY','HERMES_SECRET','PYTHONPATH','PYTHONHOME','MUSE_CONFIG_DIR','MUSE_VM_ID'):
 assert forbidden not in os.environ
r = json.load(sys.stdin)
if r['op'] == 'connect': data = {'identity_name':'User','vm_id':'VM','threads':[]}
elif r['op'] == 'history': data = []
elif r['op'] == 'tasks': data = {'schedules':[],'runs':[]}
else: raise AssertionError('must not send')
print(json.dumps({'ok':True,'data':data}))
"#,
    );
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_account-probe"));
    command
        .env_clear()
        .env("HOME", &f.0)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("MUSE_MCP_ENABLE_RAW", "1")
        .args([
            "/usr/bin/python3",
            f.0.join("bridge.py").to_str().unwrap(),
            f.0.to_str().unwrap(),
        ]);
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "HERMES_SECRET",
        "PYTHONPATH",
        "PYTHONHOME",
        "MUSE_CONFIG_DIR",
        "MUSE_VM_ID",
    ] {
        command.env(key, "SYNTHETIC_SECRET");
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "probe rejected safe fake bridge");
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "connected=true threads=0 history_messages=0 schedules=0 runs=0\n"
    );
}

#[test]
fn connect_sends_json_via_stdin_and_parses_typed_snapshot() {
    let f = Fixture::new(
        r#"
import json, sys
assert len(sys.argv) == 1
request = json.load(sys.stdin)
assert request['op'] == 'connect'
assert request['config_dir'].endswith('/config')
print(json.dumps({'ok': True, 'data': {'identity_name': 'User', 'vm_id': 'vm-1',
    'threads': [{'session_id':'thread-1', 'title':'Hello', 'archived':False, 'updated_at':None}]}}))
"#,
    );
    let snapshot = f.client().connect().unwrap();
    assert_eq!(snapshot.identity_name, "User");
    assert_eq!(snapshot.vm_id, "vm-1");
    assert_eq!(snapshot.threads[0].session_id, "thread-1");
}

#[test]
fn identity_verifies_the_account_without_paying_for_the_conversation_list() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'identity'
assert request['config_dir'].endswith('/config')
print(json.dumps({'ok': True, 'data': {'identity_name': 'User', 'vm_id': 'vm-1'}}))
"#,
    );
    let identity = f.client().identity().unwrap();
    assert_eq!(identity.identity_name, "User");
    assert_eq!(identity.vm_id, "vm-1");
}

#[test]
fn watch_holds_the_request_and_parses_the_window() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'watch' and request['limit'] == 20 and request['timeout'] == 15000
assert request['thread'] is None
print(json.dumps({'ok': True, 'data': [{'message_id':'m','seq':2,'role':'assistant',
    'text':'done','timestamp':None,'status':'complete'}]}))
"#,
    );
    let rows = f
        .client()
        .watch(None, 20, std::time::Duration::from_secs(15))
        .unwrap();
    assert_eq!(rows[0].message_id, "m");
    assert_eq!(rows[0].status.as_deref(), Some("complete"));
    for bad in [
        std::time::Duration::ZERO,
        std::time::Duration::from_secs(61),
    ] {
        assert!(f.client().watch(None, 20, bad).is_err());
    }
    assert!(
        f.client()
            .watch(None, 0, std::time::Duration::from_secs(1))
            .is_err()
    );
}

#[test]
fn a_changed_account_is_reported_before_dispatch_without_uncertainty() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'send'
assert request['expect_vm_id'] == 'vm-1' and request['expect_identity_name'] == 'User'
print(json.dumps({'ok': False, 'error': {'code': 'account_changed', 'uncertain': False}}))
"#,
    );
    let error = f
        .client()
        .send_verified(None, "hello", "vm-1", "User")
        .unwrap_err();
    assert_eq!(error.code, muse_account::ErrorCode::AccountChanged);
    assert!(!error.uncertain);
}

#[test]
fn identity_surfaces_a_signed_out_session_as_auth_required() {
    let f = Fixture::new(
        r#"
import json, sys
assert json.load(sys.stdin)['op'] == 'identity'
print(json.dumps({'ok': False, 'error': {'code': 'auth_required', 'uncertain': False}}))
"#,
    );
    let error = f.client().identity().unwrap_err();
    assert_eq!(error.code, muse_account::ErrorCode::AuthRequired);
    assert!(!error.uncertain);
}

#[test]
fn memory_search_sends_the_probed_body_and_projects_citations() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'memory_search'
assert request['query'] == 'budget'
assert 'q' not in request
print(json.dumps({'ok': True, 'data': {'recognized': 1, 'skipped': 1, 'hits': [
  {'memory_uri': 'memory://md:memory/2026-10-22.md:950', 'text': 'budget surgery',
   'path': 'memory/2026-10-22.md', 'line_start': 950, 'line_end': 950,
   'source_kind': 'daily', 'status': 'active', 'scores': {'final': 0.5}}]}}))
"#,
    );
    let found = f.client().memory_search("budget").unwrap();
    assert_eq!(found.recognized, 1);
    assert_eq!(found.skipped, 1);
    let row = &found.hits[0];
    assert_eq!(row.memory_uri, "memory://md:memory/2026-10-22.md:950");
    assert_eq!(row.text, "budget surgery");
    assert_eq!(row.path.as_deref(), Some("memory/2026-10-22.md"));
    assert_eq!(row.line_start, Some(950));
    assert_eq!(row.line_end, Some(950));
    assert_eq!(row.source_kind.as_deref(), Some("daily"));
    assert_eq!(row.status.as_deref(), Some("active"));
    // The service refuses an empty query, so the client never sends one.
    assert!(f.client().memory_search("").is_err());
    assert!(f.client().memory_search(&"x".repeat(513)).is_err());
}

#[test]
fn space_export_carries_the_html_and_refuses_a_bad_slug_locally() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'space_export'
assert request['slug'] == 'my-space'
print(json.dumps({'ok': True, 'data': {'slug': 'my-space', 'format': 'html',
  'size': 42, 'truncated': False, 'html': '<html><body>space</body></html>'}}))
"#,
    );
    let answer = f.client().space_export("my-space").unwrap();
    assert_eq!(answer.slug, "my-space");
    assert_eq!(answer.format, "html");
    assert_eq!(answer.size, 42);
    assert!(!answer.truncated);
    assert_eq!(
        answer.html.as_deref(),
        Some("<html><body>space</body></html>")
    );
    // A slug is one path segment from the listing's own open target;
    // anything else is refused before it is sent.
    assert!(f.client().space_export("").is_err());
    assert!(f.client().space_export("a/b").is_err());
    assert!(f.client().space_export(&"x".repeat(257)).is_err());
}

#[test]
fn policy_settings_project_pairs_and_connector_reset_validates_the_name() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
if request['op'] == 'policy_settings':
    print(json.dumps({'ok': True, 'data': {
      'entries': [{'label': 'hitl.enabled', 'value': 'true'}],
      'connectors': ['gmail'], 'withheld': 2}}))
elif request['op'] == 'connector_reset':
    assert request['connector'] == 'gmail'
    print(json.dumps({'ok': True, 'data': {'connector': 'gmail', 'accepted': True}}))
else:
    raise AssertionError(request['op'])
"#,
    );
    let settings = f.client().policy_settings().unwrap();
    assert_eq!(settings.entries.len(), 1);
    assert_eq!(settings.entries[0].label, "hitl.enabled");
    assert_eq!(settings.entries[0].value, "true");
    assert_eq!(settings.connectors, ["gmail"]);
    assert_eq!(settings.withheld, 2);
    let answer = f.client().connector_reset("gmail").unwrap();
    assert_eq!(answer.connector, "gmail");
    assert!(answer.accepted);
    // A connector name is one path segment from the settings' own
    // section; anything else is refused before it is sent.
    assert!(f.client().connector_reset("").is_err());
    assert!(f.client().connector_reset("a/b").is_err());
    assert!(f.client().connector_reset("has space").is_err());
    assert!(f.client().connector_reset("-leading").is_err());
    assert!(f.client().connector_reset(&"x".repeat(129)).is_err());
}

#[test]
fn memory_get_keeps_a_missing_entry_as_an_answer_not_a_failure() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'memory_get'
assert request['memory_uri'] == 'memory://md:memory/2026-10-22.md:950'
print(json.dumps({'ok': True, 'data': {'found': False,
  'memory_uri': request['memory_uri'], 'path': None, 'line_start': None,
  'line_end': None, 'text': None}}))
"#,
    );
    let entry = f
        .client()
        .memory_get("memory://md:memory/2026-10-22.md:950")
        .unwrap();
    assert!(!entry.found, "a gone entry is data, not an error");
    assert!(entry.text.is_none());
    // Only a URI the service published is addressable; a path is not.
    assert!(
        f.client()
            .memory_get("workspace/memory/2026-10-22.md")
            .is_err()
    );
    assert!(f.client().memory_get("").is_err());
}

#[test]
fn attach_probe_sends_one_named_thread_and_parses_the_verdicts() {
    let f = Fixture::new(
        r#"
import json, sys
request = json.load(sys.stdin)
assert request['op'] == 'attach_probe'
assert request['thread'] == 'thread-9'
print(json.dumps({'ok': True, 'data': {'thread': 'thread-9',
  'fs_write': 'accepted', 'sent': True, 'message_seen': True,
  'attachment_seen': False}}))
"#,
    );
    let answer = f.client().attach_probe("thread-9").unwrap();
    assert_eq!(answer.thread, "thread-9");
    assert_eq!(answer.fs_write, "accepted");
    assert!(answer.sent);
    assert!(answer.message_seen);
    assert!(!answer.attachment_seen);
    // The probe runs in a designated thread only; an unnamed or
    // malformed target is refused before anything is sent.
    assert!(f.client().attach_probe("").is_err());
    assert!(f.client().attach_probe(&"x".repeat(257)).is_err());
}
