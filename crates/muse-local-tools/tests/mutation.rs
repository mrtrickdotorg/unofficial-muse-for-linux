use muse_local_tools::*;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn call(b: &ToolBroker, id: &str, command: &str, params: serde_json::Value) -> Decision {
    b.invoke(Invocation {
        id: id.into(),
        command: command.into(),
        params,
        timeout_ms: 10000,
    })
}

fn propose(b: &ToolBroker, id: &str, command: &str, params: serde_json::Value) -> ApprovalRequest {
    match call(b, id, command, params) {
        Decision::Approval(p) => p,
        other => panic!("{command} must prompt, got {other:?}"),
    }
}

fn edit(b: &ToolBroker, g: &GrantInfo, id: &str, path: &str, old: &str, new: &str) -> Decision {
    call(
        b,
        id,
        "files.edit",
        json!({"grant_id":g.id,"path":path,"old_text":old,"new_text":new}),
    )
}

#[test]
fn edit_requires_approval_and_applies_exactly_one_occurrence() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("note"), "alpha beta gamma\n").unwrap();
    fs::set_permissions(d.path().join("note"), fs::Permissions::from_mode(0o640)).unwrap();
    let b = ToolBroker::new();
    let g = b.grant(d.path()).unwrap();
    let Decision::Approval(p) = edit(&b, &g, "e1", "note", "beta", "delta") else {
        panic!("must prompt");
    };
    // Nothing is written before the person allows it.
    assert_eq!(
        fs::read_to_string(d.path().join("note")).unwrap(),
        "alpha beta gamma\n"
    );
    assert_eq!(p.operation, "files.edit");
    assert_eq!(p.path, "note");
    assert_eq!(p.content, "alpha delta gamma\n");
    assert_eq!(p.target_path, None);
    assert!(b.approve(&p.approval_id).ok);
    assert_eq!(
        fs::read_to_string(d.path().join("note")).unwrap(),
        "alpha delta gamma\n"
    );
    // An edit keeps the file's own permission bits.
    assert_eq!(
        fs::metadata(d.path().join("note"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    // The approval is consumed exactly once.
    assert!(!b.approve(&p.approval_id).ok);
}

#[test]
fn edit_refuses_missing_or_ambiguous_text() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("two"), "same same").unwrap();
    fs::write(d.path().join("one"), "only once").unwrap();
    let b = ToolBroker::new();
    let g = b.grant(d.path()).unwrap();
    for (id, path, old, code) in [
        ("none", "one", "absent", "no_match"),
        ("twice", "two", "same", "ambiguous_match"),
        ("empty", "one", "", "invalid_params"),
    ] {
        match edit(&b, &g, id, path, old, "x") {
            Decision::Reply(r) => {
                assert_eq!(r.error.as_deref(), Some(code), "{id}");
                assert!(r.output.is_null());
            }
            other => panic!("{id} must fail, got {other:?}"),
        }
    }
    assert_eq!(
        fs::read_to_string(d.path().join("two")).unwrap(),
        "same same"
    );
    assert_eq!(
        fs::read_to_string(d.path().join("one")).unwrap(),
        "only once"
    );
}

#[test]
fn edit_fails_when_the_file_changed_after_approval() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("note"), "original").unwrap();
    let b = ToolBroker::new();
    let g = b.grant(d.path()).unwrap();
    let Decision::Approval(p) = edit(&b, &g, "e", "note", "original", "edited") else {
        panic!()
    };
    // A decision detail that changes after the request invalidates it.
    fs::write(d.path().join("note"), "changed by someone else").unwrap();
    assert_eq!(
        b.approve(&p.approval_id).error.as_deref(),
        Some("changed_since_approval")
    );
    assert_eq!(
        fs::read_to_string(d.path().join("note")).unwrap(),
        "changed by someone else"
    );
}

#[test]
fn rename_moves_without_overwriting_and_refuses_escapes() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("sub")).unwrap();
    fs::write(d.path().join("a"), "payload").unwrap();
    fs::write(d.path().join("sub/taken"), "keep").unwrap();
    let b = ToolBroker::new();
    let g = b.grant(d.path()).unwrap();
    let p = propose(
        &b,
        "r1",
        "files.rename",
        json!({"grant_id":g.id,"path":"a","new_path":"sub/b"}),
    );
    assert_eq!(p.operation, "files.rename");
    assert_eq!(p.target_path.as_deref(), Some("sub/b"));
    assert_eq!(p.content, "");
    assert!(d.path().join("a").exists(), "no move before approval");
    let reply = b.approve(&p.approval_id);
    assert!(reply.ok, "{:?}", reply.error);
    assert_eq!(reply.output["path"], "a");
    assert_eq!(reply.output["new_path"], "sub/b");
    assert!(!d.path().join("a").exists());
    assert_eq!(
        fs::read_to_string(d.path().join("sub/b")).unwrap(),
        "payload"
    );

    // An existing destination is never replaced.
    let q = propose(
        &b,
        "r2",
        "files.rename",
        json!({"grant_id":g.id,"path":"sub/b","new_path":"sub/taken"}),
    );
    assert_eq!(
        b.approve(&q.approval_id).error.as_deref(),
        Some("rename_denied")
    );
    assert_eq!(
        fs::read_to_string(d.path().join("sub/taken")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::read_to_string(d.path().join("sub/b")).unwrap(),
        "payload"
    );

    // Escapes and a missing destination folder are refused before any prompt.
    for (id, new_path) in [
        ("escape", "../outside"),
        ("absolute", "/tmp/elsewhere"),
        ("missing", "absent/b"),
    ] {
        match call(
            &b,
            id,
            "files.rename",
            json!({"grant_id":g.id,"path":"sub/b","new_path":new_path}),
        ) {
            Decision::Reply(r) => assert!(!r.ok, "{id}"),
            other => panic!("{id} must fail, got {other:?}"),
        }
    }
}

#[test]
fn delete_moves_to_trash_with_a_readable_record_and_never_unlinks() {
    let d = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fs::write(d.path().join("note.txt"), "recoverable").unwrap();
    // The Trash location is discovered through XDG_DATA_HOME; the crate never
    // accepts a caller-supplied trash path.
    std::env::set_var("XDG_DATA_HOME", data.path());
    let b = ToolBroker::new();
    let g = b.grant(d.path()).unwrap();
    let p = propose(
        &b,
        "d1",
        "files.delete",
        json!({"grant_id":g.id,"path":"note.txt"}),
    );
    assert_eq!(p.operation, "files.delete");
    assert_eq!(p.content, "");
    assert!(
        d.path().join("note.txt").exists(),
        "nothing before approval"
    );
    let reply = b.approve(&p.approval_id);
    assert!(reply.ok, "{:?}", reply.error);
    assert_eq!(reply.output["trashed_as"], "note.txt");
    assert!(!d.path().join("note.txt").exists());
    let files = data.path().join("Trash/files");
    let info = data.path().join("Trash/info");
    assert_eq!(
        fs::read_to_string(files.join("note.txt")).unwrap(),
        "recoverable"
    );
    let record = fs::read_to_string(info.join("note.txt.trashinfo")).unwrap();
    assert!(record.starts_with("[Trash Info]\n"), "{record}");
    let absolute = d.path().join("note.txt");
    assert!(
        record.contains(&format!("Path={}", absolute.display())),
        "{record}"
    );
    assert!(
        record.lines().any(|line| line.starts_with("DeletionDate=")
            && line.len() == "DeletionDate=YYYY-MM-DDTHH:MM:SS".len()),
        "{record}"
    );

    // A second file with the same name gets a distinct, still-recoverable slot.
    fs::write(d.path().join("note.txt"), "second").unwrap();
    let q = propose(
        &b,
        "d2",
        "files.delete",
        json!({"grant_id":g.id,"path":"note.txt"}),
    );
    let second = b.approve(&q.approval_id);
    assert!(second.ok, "{:?}", second.error);
    assert_eq!(second.output["trashed_as"], "note.txt.1");
    assert!(files.join("note.txt.1").exists());
    assert_eq!(
        fs::read_to_string(files.join("note.txt")).unwrap(),
        "recoverable",
        "the earlier trashed file is untouched"
    );
}

#[test]
fn delete_refuses_directories_and_a_denial_leaves_the_file() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("dir")).unwrap();
    fs::write(d.path().join("keep.txt"), "keep").unwrap();
    let b = ToolBroker::new();
    let g = b.grant(d.path()).unwrap();
    match call(
        &b,
        "dir",
        "files.delete",
        json!({"grant_id":g.id,"path":"dir"}),
    ) {
        Decision::Reply(r) => assert!(!r.ok),
        other => panic!("a directory must not be trashed: {other:?}"),
    }
    assert!(d.path().join("dir").exists());
    let p = propose(
        &b,
        "keep",
        "files.delete",
        json!({"grant_id":g.id,"path":"keep.txt"}),
    );
    assert_eq!(b.deny(&p.approval_id).error.as_deref(), Some("denied"));
    assert_eq!(
        fs::read_to_string(d.path().join("keep.txt")).unwrap(),
        "keep"
    );
}
