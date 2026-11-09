use muse_local_tools::*;
use serde_json::json;
fn create(b: &ToolBroker, g: &GrantInfo, id: &str, path: &str, content: &str, ms: u64) -> Decision {
    b.invoke(Invocation {
        id: id.into(),
        command: "files.create_text".into(),
        params: json!({"grant_id":g.id,"path":path,"content":content}),
        timeout_ms: ms,
    })
}
#[test]
fn create_requires_exact_once_approval() {
    let dir = tempfile::tempdir().unwrap();
    let b = ToolBroker::new();
    let g = b.grant(dir.path()).unwrap();
    let Decision::Approval(mut proposal) = create(&b, &g, "new", "note", "exact", 1000) else {
        panic!("must prompt")
    };
    assert!(!dir.path().join("note").exists());
    proposal.content = "tampered".into();
    proposal.path = "other".into();
    assert!(b.approve(&proposal.approval_id).ok);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note")).unwrap(),
        "exact"
    );
    assert!(!b.approve(&proposal.approval_id).ok);
    assert!(matches!(create(&b,&g,"new","again","x",1000),Decision::Reply(r) if !r.ok));
    assert!(!dir.path().join("again").exists());
}
#[test]
fn denied_expired_and_revoked_proposals_cannot_commit() {
    let dir = tempfile::tempdir().unwrap();
    let b = ToolBroker::new();
    let g = b.grant(dir.path()).unwrap();
    let Decision::Approval(p) = create(&b, &g, "deny", "denied", "secret", 1000) else {
        panic!()
    };
    assert_eq!(b.deny(&p.approval_id).error.as_deref(), Some("denied"));
    assert!(!b.approve(&p.approval_id).ok);
    let Decision::Approval(p) = create(&b, &g, "expire", "expired", "secret", 20) else {
        panic!()
    };
    std::thread::sleep(std::time::Duration::from_millis(30));
    let expired = b.expire();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].id, "expire");
    assert!(!b.approve(&p.approval_id).ok);
    let Decision::Approval(p) = create(&b, &g, "revoke", "revoked", "secret", 1000) else {
        panic!()
    };
    let revoked = b.revoke_all();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].id, "revoke");
    assert!(!b.approve(&p.approval_id).ok);
    assert!(b.grants().is_empty());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
#[test]
fn stale_approval_from_other_broker_cannot_authorize_new_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let old = ToolBroker::new();
    let new = ToolBroker::new();
    let a = old.grant(dir.path()).unwrap();
    let b = new.grant(dir.path()).unwrap();
    let Decision::Approval(p) = create(&old, &a, "a", "old", "old", 1000) else {
        panic!()
    };
    let Decision::Approval(q) = create(&new, &b, "b", "new", "new", 1000) else {
        panic!()
    };
    assert_ne!(p.approval_id, q.approval_id);
    assert!(!new.approve(&p.approval_id).ok);
    assert!(!dir.path().join("new").exists());
    assert!(q.expires_in_seconds > 0);
}
#[test]
fn expiry_is_enforced_even_without_timer_and_budgets_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    let b = ToolBroker::new();
    let g = b.grant(dir.path()).unwrap();
    let Decision::Approval(p) = create(&b, &g, "expire-direct", "a", "x", 20) else {
        panic!()
    };
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(b.approve(&p.approval_id).error.as_deref(), Some("expired"));
    assert!(
        matches!(create(&b,&g,"zero","a","x",0),Decision::Reply(r) if r.error.as_deref()==Some("expired"))
    );
    let Decision::Approval(p) = create(&b, &g, "cap", "a", "x", u64::MAX) else {
        panic!()
    };
    assert!(p.expires_in_seconds <= 120);
    assert!(p.expires_in_seconds > 0);
    assert!(b.clone().deny(&p.approval_id).error.is_some());
    assert!(!b.approve(&p.approval_id).ok);
    assert!(!dir.path().join("a").exists());
}
#[test]
fn new_file_can_be_exactly_64k_in_existing_parent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let b = ToolBroker::new();
    let g = b.grant(dir.path()).unwrap();
    let text = "é".repeat(32768);
    let Decision::Approval(p) = create(&b, &g, "max", "sub/file", &text, 10000) else {
        panic!()
    };
    assert!(b.approve(&p.approval_id).ok);
    assert_eq!(
        std::fs::read(dir.path().join("sub/file")).unwrap(),
        text.as_bytes()
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(dir.path().join("sub/file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
#[test]
fn concurrent_creates_never_replace_winner() {
    let d = tempfile::tempdir().unwrap();
    let b = ToolBroker::new();
    let g = b.grant(d.path()).unwrap();
    let Decision::Approval(a) = create(&b, &g, "one", "file", "one", 10000) else {
        panic!()
    };
    let Decision::Approval(z) = create(&b, &g, "two", "file", "two", 10000) else {
        panic!()
    };
    let worker = b.clone();
    let t = std::thread::spawn(move || worker.approve(&a.approval_id));
    let second = b.approve(&z.approval_id);
    let first = t.join().unwrap();
    assert_ne!(first.ok, second.ok);
    assert_eq!(
        std::fs::read_to_string(d.path().join("file")).unwrap(),
        if first.ok { "one" } else { "two" }
    );
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
}
#[test]
fn bounded_pending_and_id_cache_fail_closed_without_eviction() {
    let d = tempfile::tempdir().unwrap();
    let b = ToolBroker::new();
    let g = b.grant(d.path()).unwrap();
    for i in 0..128 {
        assert!(matches!(
            create(&b, &g, &format!("p{i}"), "new", "x", 120000),
            Decision::Approval(_)
        ));
    }
    assert!(
        matches!(create(&b,&g,"full","new","x",120000),Decision::Reply(r) if r.error.as_deref()==Some("capacity"))
    );
    assert_eq!(b.revoke_all().len(), 128);
    for i in 129..4096 {
        let _ = create(&b, &g, &format!("p{i}"), "new", "x", 1000);
    }
    assert!(
        matches!(create(&b,&g,"p0","new","x",1000),Decision::Reply(r) if r.error.as_deref()==Some("duplicate_id"))
    );
    assert!(
        matches!(create(&b,&g,"unseen","new","x",1000),Decision::Reply(r) if r.error.as_deref()==Some("capacity"))
    );
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
}

#[test]
fn a_read_only_grant_reads_but_refuses_every_mutation_before_proposal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "original").unwrap();
    let b = ToolBroker::new();
    let g = b.grant_read_only(dir.path()).unwrap();
    assert!(!g.writable);
    // The manifest tells the agent the mode up front, so a refused mutation
    // is never a surprise it has to discover by trying.
    let manifest = b.manifest();
    assert_eq!(manifest["grants"][0]["access"], "read_only");

    // Reads and search still run immediately.
    let read = b.invoke(Invocation {
        id: "r1".into(),
        command: "files.read_text".into(),
        params: json!({"grant_id":g.id,"path":"keep.txt"}),
        timeout_ms: 1000,
    });
    assert!(matches!(read, Decision::Reply(r) if r.ok));
    let search = b.invoke(Invocation {
        id: "r2".into(),
        command: "files.search".into(),
        params: json!({"grant_id":g.id,"query":"keep"}),
        timeout_ms: 1000,
    });
    assert!(matches!(search, Decision::Reply(r) if r.ok));

    // Every mutation is a refusal, never an approval request.
    for (id, command, params) in [
        (
            "m1",
            "files.create_text",
            json!({"grant_id":g.id,"path":"new.txt","content":"x"}),
        ),
        (
            "m2",
            "files.edit",
            json!({"grant_id":g.id,"path":"keep.txt","old_text":"original","new_text":"changed"}),
        ),
        (
            "m3",
            "files.rename",
            json!({"grant_id":g.id,"path":"keep.txt","new_path":"moved.txt"}),
        ),
        (
            "m4",
            "files.delete",
            json!({"grant_id":g.id,"path":"keep.txt"}),
        ),
    ] {
        let decision = b.invoke(Invocation {
            id: id.into(),
            command: command.into(),
            params,
            timeout_ms: 1000,
        });
        assert!(
            matches!(decision, Decision::Reply(ref r) if !r.ok && r.error.as_deref() == Some("read_only")),
            "{command} under a read-only grant must be refused, got {decision:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
        "original"
    );
    assert!(!dir.path().join("new.txt").exists());
}

#[test]
fn a_read_write_grant_still_proposes_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let b = ToolBroker::new();
    let g = b.grant(dir.path()).unwrap();
    assert!(g.writable);
    assert_eq!(b.manifest()["grants"][0]["access"], "read_write");
    assert!(matches!(
        create(&b, &g, "w1", "note", "x", 1000),
        Decision::Approval(_)
    ));
}

#[test]
fn content_search_finds_lines_and_skips_what_it_cannot_read_as_text() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("notes.txt"),
        "first line\nthe needle is here\nlast needle line\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/deep.txt"), "deep needle\n").unwrap();
    std::fs::write(
        dir.path().join("bin.dat"),
        [0xff, 0xfe, b'n', b'e', b'e', b'd', b'l', b'e'],
    )
    .unwrap();
    let b = ToolBroker::new();
    let g = b.grant_read_only(dir.path()).unwrap();
    let Decision::Reply(reply) = b.invoke(Invocation {
        id: "c1".into(),
        command: "files.search_content".into(),
        params: json!({"grant_id":g.id,"query":"needle"}),
        timeout_ms: 5000,
    }) else {
        panic!("content search is a read: it must reply, never propose");
    };
    assert!(reply.ok, "{reply:?}");
    let matches = reply.output["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 3, "{matches:?}");
    assert!(matches
        .iter()
        .any(|m| m["path"] == "notes.txt" && m["line"] == 2 && m["text"] == "the needle is here"));
    assert!(matches
        .iter()
        .any(|m| m["path"] == "notes.txt" && m["line"] == 3));
    assert!(matches
        .iter()
        .any(|m| m["path"] == "sub/deep.txt" && m["line"] == 1));
    assert_eq!(reply.output["truncated"], false);
    // Case-sensitive, like filename search.
    let Decision::Reply(none) = b.invoke(Invocation {
        id: "c2".into(),
        command: "files.search_content".into(),
        params: json!({"grant_id":g.id,"query":"NEEDLE"}),
        timeout_ms: 5000,
    }) else {
        panic!("must reply");
    };
    assert!(none.ok && none.output["matches"].as_array().unwrap().is_empty());
    // An empty query is a malformed call, not a match-everything search.
    let Decision::Reply(bad) = b.invoke(Invocation {
        id: "c3".into(),
        command: "files.search_content".into(),
        params: json!({"grant_id":g.id,"query":""}),
        timeout_ms: 5000,
    }) else {
        panic!("must reply");
    };
    assert!(!bad.ok && bad.error.as_deref() == Some("invalid_params"));
}

#[test]
fn cross_folder_move_approves_into_the_second_grant() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("note.txt"), "move me").unwrap();
    std::fs::create_dir(b.path().join("inbox")).unwrap();
    let broker = ToolBroker::new();
    let ga = broker.grant(a.path()).unwrap();
    let gb = broker.grant(b.path()).unwrap();
    let Decision::Approval(request) = broker.invoke(Invocation {
        id: "x1".into(),
        command: "files.rename".into(),
        params: json!({"grant_id":ga.id,"path":"note.txt","new_path":"inbox/note.txt","target_grant_id":gb.id}),
        timeout_ms: 5000,
    }) else {
        panic!("a cross-folder move must propose");
    };
    assert_eq!(request.operation, "files.rename");
    assert_eq!(request.title, "Move a file to another granted folder");
    let reply = broker.approve(&request.approval_id);
    assert!(reply.ok, "{reply:?}");
    assert!(!a.path().join("note.txt").exists());
    assert_eq!(
        std::fs::read_to_string(b.path().join("inbox/note.txt")).unwrap(),
        "move me"
    );
    // An existing destination name in the other folder is refused at commit,
    // never overwritten; an unknown destination grant is not_granted; a
    // read-only destination refuses before any proposal exists.
    std::fs::write(a.path().join("note.txt"), "again").unwrap();
    let Decision::Approval(second) = broker.invoke(Invocation {
        id: "x2".into(),
        command: "files.rename".into(),
        params: json!({"grant_id":ga.id,"path":"note.txt","new_path":"inbox/note.txt","target_grant_id":gb.id}),
        timeout_ms: 5000,
    }) else {
        panic!("must propose");
    };
    let reply = broker.approve(&second.approval_id);
    assert!(
        !reply.ok && reply.error.as_deref() == Some("rename_denied"),
        "{reply:?}"
    );
    assert_eq!(
        std::fs::read_to_string(b.path().join("inbox/note.txt")).unwrap(),
        "move me"
    );
    let Decision::Reply(unknown) = broker.invoke(Invocation {
        id: "x3".into(),
        command: "files.rename".into(),
        params: json!({"grant_id":ga.id,"path":"note.txt","new_path":"x.txt","target_grant_id":"grant-nope"}),
        timeout_ms: 5000,
    }) else {
        panic!("must reply");
    };
    assert!(!unknown.ok && unknown.error.as_deref() == Some("not_granted"));
    let ro = ToolBroker::new();
    let src = ro.grant(a.path()).unwrap();
    let dst = ro.grant_read_only(b.path()).unwrap();
    let Decision::Reply(refused) = ro.invoke(Invocation {
        id: "x4".into(),
        command: "files.rename".into(),
        params: json!({"grant_id":src.id,"path":"note.txt","new_path":"inbox/note.txt","target_grant_id":dst.id}),
        timeout_ms: 5000,
    }) else {
        panic!("a read-only destination must refuse outright, never propose");
    };
    assert!(!refused.ok && refused.error.as_deref() == Some("read_only"));
    // Naming the source folder as its own target is a plain rename.
    let Decision::Approval(plain) = broker.invoke(Invocation {
        id: "x5".into(),
        command: "files.rename".into(),
        params: json!({"grant_id":ga.id,"path":"note.txt","new_path":"renamed.txt","target_grant_id":ga.id}),
        timeout_ms: 5000,
    }) else {
        panic!("must propose");
    };
    assert_eq!(plain.title, "Rename a file");
    assert!(broker.approve(&plain.approval_id).ok);
    assert!(a.path().join("renamed.txt").exists());
}

#[test]
fn manifest_exposes_content_search_and_the_optional_move_target() {
    let dir = tempfile::tempdir().unwrap();
    let b = ToolBroker::new();
    let g = b.grant(dir.path()).unwrap();
    let manifest = b.manifest();
    let commands = &manifest["commands_v2"];
    assert!(commands["files.search_content"].is_object());
    let rename = &commands["files.rename"];
    assert_eq!(rename["optional"]["target_grant_id"]["type"], "string");
    assert_eq!(
        rename["parameters"]["properties"]["target_grant_id"]["type"],
        "string"
    );
    let required: Vec<&str> = rename["parameters"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, ["grant_id", "path", "new_path"]);
    // The grant enum on the optional parameter lists the live grant.
    let ids: Vec<&str> = rename["optional"]["target_grant_id"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(ids, [g.id.as_str()]);
}
