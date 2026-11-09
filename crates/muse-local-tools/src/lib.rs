//! Confined Linux filesystem capabilities for an explicitly authorized Muse host.
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const LIMIT: usize = 65536;
#[derive(Clone, Debug)]
pub struct GrantInfo {
    pub id: String,
    pub label: String,
    pub root: PathBuf,
    /// Whether mutation commands may be proposed under this grant. A
    /// read-only grant is a different promise from a read-write one: reads
    /// and search work, and every mutation is refused by the broker before
    /// it can become an approval request — the person is never asked to
    /// police a boundary they already set.
    pub writable: bool,
}
#[derive(Clone, Debug)]
pub struct Invocation {
    pub id: String,
    pub command: String,
    pub params: Value,
    pub timeout_ms: u64,
}
#[derive(Clone, Debug)]
pub struct ToolReply {
    pub id: String,
    pub ok: bool,
    pub output: Value,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub invocation_id: String,
    /// The command being approved, so a card can say what it will do.
    pub operation: String,
    pub title: String,
    pub path: String,
    /// Destination of a rename; `None` for every other operation.
    pub target_path: Option<String>,
    /// The exact resulting text for create and edit; empty for rename/delete.
    pub content: String,
    pub expires_in_seconds: u64,
}
#[derive(Clone, Debug)]
pub enum Decision {
    Reply(ToolReply),
    Approval(ApprovalRequest),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolError(pub &'static str);
impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ToolError {}
struct Grant {
    info: GrantInfo,
    fd: Arc<File>,
}
struct Pending {
    request: ApprovalRequest,
    root: Arc<File>,
    /// The granted path, used only for a Trash record's absolute origin.
    root_path: PathBuf,
    /// Destination root for a cross-folder rename; None means the
    /// destination lives under `root` like every other operation.
    target_root: Option<Arc<File>>,
    /// Exact file bytes at proposal time, for an edit that must not clobber a
    /// file the person never saw.
    before: Option<Vec<u8>>,
    deadline: Instant,
    epoch: u64,
}
#[derive(Default)]
struct State {
    grants: HashMap<String, Grant>,
    seen: HashSet<String>,
    pending: HashMap<String, Pending>,
    nonce: String,
    epoch: u64,
    next: u64,
}
#[cfg(test)]
type Pause = Arc<(std::sync::Barrier, std::sync::Barrier)>;
#[derive(Clone)]
pub struct ToolBroker {
    state: Arc<Mutex<State>>,
    #[cfg(test)]
    pause: Arc<Mutex<Option<Pause>>>,
}
fn fail(id: &str, code: &str) -> ToolReply {
    ToolReply {
        id: id.into(),
        ok: false,
        output: Value::Null,
        error: Some(code.into()),
    }
}
fn good(id: &str, output: Value) -> ToolReply {
    ToolReply {
        id: id.into(),
        ok: true,
        output,
        error: None,
    }
}
fn cstr(s: &str) -> Result<CString, ToolError> {
    CString::new(s).map_err(|_| ToolError("invalid_path"))
}
fn valid_path(s: &str) -> Result<(), ToolError> {
    if s.is_empty()
        || s.len() > 4096
        || s.split('/').any(|x| x.is_empty() || x == "." || x == "..")
        || s.contains('\0')
    {
        Err(ToolError("invalid_path"))
    } else {
        Ok(())
    }
}
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn open_beneath(root: &File, path: &str, directory: bool) -> Result<File, ToolError> {
    valid_path(path)?;
    let path = cstr(path)?;
    let how = OpenHow {
        flags: (libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 }) as u64,
        mode: 0,
        resolve: 0x08 | 0x04 | 0x02,
    };
    // SAFETY: pointers refer to initialized, live buffers; returned fd is uniquely owned.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(ToolError("io_denied"));
    }
    let file = unsafe { File::from_raw_fd(fd as i32) };
    if !directory
        && !file
            .metadata()
            .map_err(|_| ToolError("io_denied"))?
            .is_file()
    {
        return Err(ToolError("special_file"));
    }
    Ok(file)
}
fn read_bytes(root: &File, path: &str) -> Result<Vec<u8>, ToolError> {
    let file = open_beneath(root, path, false)?;
    if file.metadata().map_err(|_| ToolError("io_denied"))?.len() > LIMIT as u64 {
        return Err(ToolError("too_large"));
    }
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ToolError("io_denied"))?;
    if bytes.len() > LIMIT {
        return Err(ToolError("too_large"));
    }
    Ok(bytes)
}
fn read_text(root: &File, path: &str) -> Result<Value, ToolError> {
    let content =
        String::from_utf8(read_bytes(root, path)?).map_err(|_| ToolError("invalid_utf8"))?;
    Ok(json!({"path":path,"content":content}))
}
impl ToolBroker {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                nonce: fresh_nonce(),
                ..State::default()
            })),
            #[cfg(test)]
            pause: Arc::default(),
        }
    }
    /// Grant a folder read-write: reads and search run immediately, and
    /// mutations may be proposed for approval.
    pub fn grant(&self, root: &Path) -> Result<GrantInfo, ToolError> {
        self.grant_inner(root, true)
    }
    /// Grant a folder read-only: reads and search run immediately, and the
    /// broker refuses every mutation under it outright.
    pub fn grant_read_only(&self, root: &Path) -> Result<GrantInfo, ToolError> {
        self.grant_inner(root, false)
    }
    fn grant_inner(&self, root: &Path, writable: bool) -> Result<GrantInfo, ToolError> {
        if root.as_os_str().is_empty() {
            return Err(ToolError("invalid_path"));
        }
        let epoch = self.state.lock().unwrap().epoch;
        // Open the supplied root without following any component's symlink.
        let base = File::open(if root.is_absolute() { "/" } else { "." })
            .map_err(|_| ToolError("io_denied"))?;
        let relative = if root.is_absolute() {
            root.strip_prefix("/")
                .map_err(|_| ToolError("invalid_path"))?
        } else {
            root
        };
        let fd = if relative.as_os_str().is_empty() {
            base
        } else {
            open_beneath(
                &base,
                relative.to_str().ok_or(ToolError("invalid_path"))?,
                true,
            )?
        };
        #[cfg(test)]
        self.test_pause();
        let mut state = self.state.lock().unwrap();
        if state.epoch != epoch {
            return Err(ToolError("revoked"));
        }
        state.next += 1;
        let info = GrantInfo {
            id: format!("grant-{}-{}", state.nonce, state.next),
            label: format!("Folder {}", state.next),
            root: root.to_path_buf(),
            writable,
        };
        state.grants.insert(
            info.id.clone(),
            Grant {
                info: info.clone(),
                fd: Arc::new(fd),
            },
        );
        Ok(info)
    }
    pub fn grants(&self) -> Vec<GrantInfo> {
        let mut grants: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .grants
            .values()
            .map(|x| x.info.clone())
            .collect();
        grants.sort_by(|a, b| a.id.cmp(&b.id));
        grants
    }
    pub fn manifest(&self) -> Value {
        let grants: Vec<_> = self
            .grants()
            .into_iter()
            .map(|g| json!({"id":g.id,"label":g.label,"access":if g.writable {"read_write"} else {"read_only"}}))
            .collect();
        let grant_ids: Vec<_> = grants.iter().map(|grant| grant["id"].clone()).collect();
        let grant_description = format!(
            "Granted folder ID. Available user-authorized folders (ID and label): {}",
            serde_json::to_string(&grants).expect("grant metadata is serializable")
        );
        let mut commands = serde_json::Map::new();
        for (name,description,fields) in [
            ("files.search","Search filenames in an explicitly granted folder.",vec![("grant_id","Granted folder ID"),("query","Case-sensitive filename substring; max 256 bytes")]),
            ("files.search_content","Search file contents in an explicitly granted folder. Returns matching lines (path, 1-based line number, line text) from UTF-8 text files only; bounded in files read, matches returned and line length.",vec![("grant_id","Granted folder ID"),("query","Case-sensitive text substring; max 256 bytes")]),
            ("files.read_text","Read at most 64 KiB of UTF-8 text inside a granted folder.",vec![("grant_id","Granted folder ID"),("path","Strict relative path")]),
            ("files.create_text","Propose a NEW UTF-8 text file. Always requires local approval of exact path and content; never overwrite.",vec![("grant_id","Granted folder ID"),("path","Strict relative new-file path; parent must exist"),("content","Exact UTF-8 text, at most 65536 bytes")]),
            ("files.edit","Propose replacing ONE exact occurrence of text in an existing UTF-8 file. Always requires local approval of the exact resulting content; fails if the text is absent, ambiguous, or the file changed after approval.",vec![("grant_id","Granted folder ID"),("path","Strict relative path to an existing file"),("old_text","Exact text to replace; must occur exactly once"),("new_text","Exact replacement text")]),
            ("files.rename","Propose renaming a file inside one granted folder, or moving it into another granted folder by also passing target_grant_id. Always requires local approval; never overwrites an existing name.",vec![("grant_id","Granted folder ID"),("path","Strict relative source path"),("new_path","Strict relative destination path; must not exist")]),
            ("files.delete","Propose moving a regular file to the desktop Trash. Always requires local approval; nothing is unlinked and the file stays recoverable.",vec![("grant_id","Granted folder ID"),("path","Strict relative path to an existing file")]),
        ] {
            let required: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(key, description)| {
                    let parameter = if *key == "grant_id" {
                        json!({"type":"string","description":grant_description,"enum":grant_ids})
                    } else {
                        json!({"type":"string","description":description})
                    };
                    (key.to_string(), parameter)
                })
                .collect();
            let names:Vec<_>=fields.iter().map(|(key,_)|*key).collect();
            // files.rename's one optional parameter: the destination folder
            // for a cross-folder move. It joins the schema's properties (so
            // the strict additionalProperties check admits it) but never the
            // required list.
            let mut optional = serde_json::Map::new();
            let mut properties = required.clone();
            if name == "files.rename" {
                let target_param = json!({"type":"string","description":"Destination granted folder ID for a move into another folder; omit to rename within the source folder","enum":grant_ids});
                optional.insert("target_grant_id".into(), target_param.clone());
                properties.insert("target_grant_id".into(), target_param);
            }
            commands.insert(name.into(),json!({"description":description,"required":required,"optional":optional,"timeout_ms":120000,"parameters":{"type":"object","properties":properties,"required":names,"additionalProperties":false}}));
        }
        json!({"commands_v2":commands,"grants":grants})
    }
    pub fn invoke(&self, invocation: Invocation) -> Decision {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(invocation.timeout_ms.min(120_000));
        let (root, root_path, epoch, writable, target_grant) = {
            let mut state = self.state.lock().unwrap();
            if invocation.id.is_empty() || invocation.id.len() > 256 {
                return Decision::Reply(fail(&invocation.id, "invalid_id"));
            }
            if state.seen.contains(&invocation.id) {
                return Decision::Reply(fail(&invocation.id, "duplicate_id"));
            }
            // Never evict accepted IDs: after capacity, reject all new IDs until broker replacement.
            if state.seen.len() >= 4096 {
                return Decision::Reply(fail(&invocation.id, "capacity"));
            }
            state.seen.insert(invocation.id.clone());
            let result = (|| {
                let keys: &[&str] = match invocation.command.as_str() {
                    "files.read_text" => &["grant_id", "path"],
                    "files.search" => &["grant_id", "query"],
                    "files.search_content" => &["grant_id", "query"],
                    "files.create_text" => &["grant_id", "path", "content"],
                    "files.edit" => &["grant_id", "path", "old_text", "new_text"],
                    "files.rename" => &["grant_id", "path", "new_path"],
                    "files.delete" => &["grant_id", "path"],
                    _ => return Err(ToolError("unknown_command")),
                };
                let params = invocation
                    .params
                    .as_object()
                    .ok_or(ToolError("invalid_params"))?;
                // files.rename alone takes one optional parameter:
                // target_grant_id names a second granted folder to move into.
                let params_valid = if invocation.command == "files.rename" {
                    (params.len() == 3 || params.len() == 4)
                        && keys
                            .iter()
                            .all(|key| params.get(*key).and_then(Value::as_str).is_some())
                        && params
                            .get("target_grant_id")
                            .is_none_or(|v| v.as_str().is_some())
                } else {
                    params.len() == keys.len()
                        && keys
                            .iter()
                            .all(|key| params.get(*key).and_then(Value::as_str).is_some())
                };
                if !params_valid {
                    return Err(ToolError("invalid_params"));
                }
                if Instant::now() >= deadline {
                    return Err(ToolError("expired"));
                }
                let grant = state
                    .grants
                    .get(params["grant_id"].as_str().unwrap())
                    .ok_or(ToolError("not_granted"))?;
                if invocation.command == "files.search" {
                    let query = params["query"].as_str().unwrap();
                    if query.len() > 256 || query.contains(['/', '\0']) {
                        return Err(ToolError("invalid_params"));
                    }
                } else if invocation.command == "files.search_content" {
                    // A content query is text, not a path: '/' is legitimate,
                    // emptiness and NUL are not.
                    let query = params["query"].as_str().unwrap();
                    if query.is_empty() || query.len() > 256 || query.contains('\0') {
                        return Err(ToolError("invalid_params"));
                    }
                } else {
                    valid_path(params["path"].as_str().unwrap())?;
                    if invocation.command == "files.rename" {
                        valid_path(params["new_path"].as_str().unwrap())?;
                    }
                }
                // A cross-folder move resolves its destination grant here,
                // under the same lock and epoch as the source: an unknown
                // destination is not_granted, exactly like an unknown source,
                // and naming the source folder again is a plain rename.
                let target = match params.get("target_grant_id").and_then(Value::as_str) {
                    Some(tid) if tid != grant.info.id => {
                        let dest = state.grants.get(tid).ok_or(ToolError("not_granted"))?;
                        Some((dest.fd.clone(), dest.info.root.clone(), dest.info.writable))
                    }
                    _ => None,
                };
                Ok((
                    grant.fd.clone(),
                    grant.info.root.clone(),
                    state.epoch,
                    grant.info.writable,
                    target,
                ))
            })();
            match result {
                Ok(v) => v,
                Err(e) => return Decision::Reply(fail(&invocation.id, e.0)),
            }
        };
        if matches!(
            invocation.command.as_str(),
            "files.create_text" | "files.edit" | "files.rename" | "files.delete"
        ) {
            // A read-only grant refuses mutations here, before any proposal
            // exists: there is no approval to ask for, because the person
            // already answered when they granted the folder. A cross-folder
            // move writes into TWO folders, so a read-only destination
            // refuses it the same way.
            if !writable || target_grant.as_ref().is_some_and(|(_, _, w)| !w) {
                return Decision::Reply(fail(&invocation.id, "read_only"));
            }
            return self.propose(invocation, root, root_path, epoch, deadline, target_grant);
        }
        let result = if invocation.command == "files.search" {
            search(
                &root,
                invocation.params["query"].as_str().unwrap(),
                deadline,
            )
        } else if invocation.command == "files.search_content" {
            search_content(
                &root,
                invocation.params["query"].as_str().unwrap(),
                deadline,
            )
        } else {
            read_text(&root, invocation.params["path"].as_str().unwrap())
        };
        #[cfg(test)]
        self.test_pause();
        let state = self.state.lock().unwrap();
        Decision::Reply(if state.epoch != epoch {
            fail(&invocation.id, "revoked")
        } else if Instant::now() >= deadline {
            fail(&invocation.id, "expired")
        } else {
            match result {
                Ok(v) => good(&invocation.id, v),
                Err(e) => fail(&invocation.id, e.0),
            }
        })
    }
    /// Turn a mutation into an approval request. Nothing is written here: an
    /// edit computes the exact resulting bytes, a rename and a Trash move only
    /// prove their preconditions, and every one of them waits for a person.
    fn propose(
        &self,
        invocation: Invocation,
        root: Arc<File>,
        root_path: PathBuf,
        epoch: u64,
        deadline: Instant,
        target_grant: Option<(Arc<File>, PathBuf, bool)>,
    ) -> Decision {
        let id = invocation.id.clone();
        let path = invocation.params["path"].as_str().unwrap().to_string();
        let (operation, title, target, content, before) = match invocation.command.as_str() {
            "files.create_text" => {
                let content = invocation.params["content"].as_str().unwrap().to_string();
                if content.len() > LIMIT {
                    return Decision::Reply(fail(&id, "too_large"));
                }
                (
                    "files.create_text",
                    "Create a new text file",
                    None,
                    content,
                    None,
                )
            }
            "files.edit" => {
                let old = invocation.params["old_text"].as_str().unwrap();
                let new = invocation.params["new_text"].as_str().unwrap();
                // An empty needle would match everywhere; a person cannot approve
                // "replace nothing" and the result would not be reviewable.
                if old.is_empty() {
                    return Decision::Reply(fail(&id, "invalid_params"));
                }
                let original = match read_bytes(&root, &path) {
                    Ok(bytes) => bytes,
                    Err(e) => return Decision::Reply(fail(&id, e.0)),
                };
                let text = match std::str::from_utf8(&original) {
                    Ok(text) => text,
                    Err(_) => return Decision::Reply(fail(&id, "invalid_utf8")),
                };
                match text.matches(old).count() {
                    0 => return Decision::Reply(fail(&id, "no_match")),
                    1 => {}
                    _ => return Decision::Reply(fail(&id, "ambiguous_match")),
                }
                let after = text.replacen(old, new, 1);
                if after.len() > LIMIT {
                    return Decision::Reply(fail(&id, "too_large"));
                }
                (
                    "files.edit",
                    "Edit a text file",
                    None,
                    after,
                    Some(original),
                )
            }
            "files.rename" => {
                let target = invocation.params["new_path"].as_str().unwrap();
                // The destination parent must exist in the DESTINATION root:
                // the source root for a plain rename, the second granted
                // folder for a cross-folder move. A missing one is a broken
                // request, not a reason to ask a person for permission that
                // cannot be exercised.
                let dest_root = target_grant.as_ref().map(|(fd, _, _)| fd).unwrap_or(&root);
                if (target_grant.is_none() && target == path)
                    || parent_of(dest_root, target).is_err()
                {
                    return Decision::Reply(fail(&id, "rename_denied"));
                }
                if open_beneath(&root, &path, false).is_err() {
                    return Decision::Reply(fail(&id, "io_denied"));
                }
                (
                    "files.rename",
                    if target_grant.is_some() {
                        "Move a file to another granted folder"
                    } else {
                        "Rename a file"
                    },
                    Some(target.to_string()),
                    String::new(),
                    None,
                )
            }
            "files.delete" => {
                if open_beneath(&root, &path, false).is_err() {
                    return Decision::Reply(fail(&id, "io_denied"));
                }
                (
                    "files.delete",
                    "Move a file to Trash",
                    None,
                    String::new(),
                    None,
                )
            }
            _ => return Decision::Reply(fail(&id, "unknown_command")),
        };
        let approval_id = format!("approval-{}", fresh_nonce());
        let mut state = self.state.lock().unwrap();
        if state.epoch != epoch {
            return Decision::Reply(fail(&id, "revoked"));
        }
        if Instant::now() >= deadline {
            return Decision::Reply(fail(&id, "expired"));
        }
        if state.pending.len() >= 128 {
            return Decision::Reply(fail(&id, "capacity"));
        }
        let request = ApprovalRequest {
            approval_id,
            invocation_id: id.clone(),
            operation: operation.into(),
            title: title.into(),
            path,
            target_path: target,
            content,
            expires_in_seconds: deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .div_ceil(1000) as u64,
        };
        state.pending.insert(
            request.approval_id.clone(),
            Pending {
                request: request.clone(),
                root,
                root_path,
                target_root: target_grant.map(|(fd, _, _)| fd),
                before,
                deadline,
                epoch,
            },
        );
        Decision::Approval(request)
    }
    pub fn deny(&self, approval_id: &str) -> ToolReply {
        match self.state.lock().unwrap().pending.remove(approval_id) {
            Some(p) => fail(&p.request.invocation_id, "denied"),
            None => fail("", "stale_approval"),
        }
    }
    pub fn revoke_all(&self) -> Vec<ToolReply> {
        let mut state = self.state.lock().unwrap();
        state.epoch += 1;
        state.grants.clear();
        let mut replies: Vec<_> = state
            .pending
            .drain()
            .map(|(_, p)| fail(&p.request.invocation_id, "revoked"))
            .collect();
        replies.sort_by(|a, b| a.id.cmp(&b.id));
        replies
    }
    pub fn expire(&self) -> Vec<ToolReply> {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();
        let ids: Vec<_> = state
            .pending
            .iter()
            .filter(|(_, p)| now >= p.deadline)
            .map(|(id, _)| id.clone())
            .collect();
        let mut replies: Vec<_> = ids
            .into_iter()
            .map(|id| {
                fail(
                    &state.pending.remove(&id).unwrap().request.invocation_id,
                    "expired",
                )
            })
            .collect();
        replies.sort_by(|a, b| a.id.cmp(&b.id));
        replies
    }
    pub fn approve(&self, approval_id: &str) -> ToolReply {
        let pending = match self.state.lock().unwrap().pending.remove(approval_id) {
            Some(p) => p,
            None => return fail("", "stale_approval"),
        };
        let id = pending.request.invocation_id.clone();
        if Instant::now() >= pending.deadline {
            return fail(&id, "expired");
        }
        // Prepare outside the state lock: nothing visible exists yet, so a revoke
        // or an expiry discards the prepared inode without leaving debris.
        let prepared = match pending.request.operation.as_str() {
            "files.create_text" => stage(
                &pending.root,
                &pending.request.path,
                &pending.request.content,
            )
            .map(Prepared::Create),
            "files.edit" => stage_edit(&pending).map(Prepared::Edit),
            "files.rename" | "files.delete" => Ok(Prepared::Direct),
            _ => Err(ToolError("unknown_command")),
        };
        #[cfg(test)]
        self.test_pause();
        let state = self.state.lock().unwrap();
        if state.epoch != pending.epoch {
            return fail(&id, "revoked");
        }
        if Instant::now() >= pending.deadline {
            return fail(&id, "expired");
        }
        let prepared = match prepared {
            Ok(v) => v,
            Err(e) => return fail(&id, e.0),
        };
        let result = match prepared {
            Prepared::Create((parent, file, name)) => publish_new(&pending, parent, file, name),
            Prepared::Edit(edit) => commit_edit(&pending, edit),
            Prepared::Direct if pending.request.operation == "files.rename" => {
                commit_rename(&pending)
            }
            Prepared::Direct if pending.request.operation == "files.delete" => {
                commit_trash(&pending)
            }
            Prepared::Direct => Err(ToolError("unknown_command")),
        };
        match result {
            Ok(output) => good(&id, output),
            Err(e) => fail(&id, e.0),
        }
    }
}

fn stage(root: &File, path: &str, content: &str) -> Result<(File, File, CString), ToolError> {
    valid_path(path)?;
    let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
    let parent = if parent_path.is_empty() {
        root.try_clone().map_err(|_| ToolError("io_denied"))?
    } else {
        open_beneath(root, parent_path, true)?
    };
    let dot = c".";
    // SAFETY: directory fd and static C string are valid; O_TMPFILE leaves no named debris.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            dot.as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(ToolError("create_denied"));
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(content.as_bytes())
        .map_err(|_| ToolError("create_denied"))?;
    file.sync_all().map_err(|_| ToolError("create_denied"))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ToolError("create_denied"))?;
    let mut actual = Vec::new();
    (&mut file)
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut actual)
        .map_err(|_| ToolError("create_denied"))?;
    if actual != content.as_bytes() {
        return Err(ToolError("verify_failed"));
    }
    Ok((parent, file, cstr(name)?))
}
enum Prepared {
    Create((File, File, CString)),
    Edit(Edit),
    Direct,
}
fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}
fn parent_of(root: &File, path: &str) -> Result<File, ToolError> {
    let (parent, _) = split_path(path);
    if parent.is_empty() {
        root.try_clone().map_err(|_| ToolError("io_denied"))
    } else {
        open_beneath(root, parent, true)
    }
}
fn publish_new(
    pending: &Pending,
    parent: File,
    file: File,
    name: CString,
) -> Result<Value, ToolError> {
    let empty = c"";
    // SAFETY: fds and C strings remain live; an anonymous verified inode is linked exactly once.
    let rc = unsafe {
        libc::linkat(
            file.as_raw_fd(),
            empty.as_ptr(),
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    };
    if rc != 0 {
        return Err(ToolError("create_denied"));
    }
    Ok(json!({"path":pending.request.path,"bytes":pending.request.content.len()}))
}
struct Edit {
    parent: File,
    temp: File,
    name: CString,
    dev: u64,
    ino: u64,
}
/// Prepare an edit without publishing it: the current bytes must still be the
/// ones the person approved, and the replacement is written, synced and read
/// back in an anonymous inode before any name changes.
fn stage_edit(pending: &Pending) -> Result<Edit, ToolError> {
    let before = pending
        .before
        .as_ref()
        .ok_or(ToolError("changed_since_approval"))?;
    let path = pending.request.path.as_str();
    let parent = parent_of(&pending.root, path)?;
    let (_, name) = split_path(path);
    let name = cstr(name)?;
    let live = open_beneath(&pending.root, path, false)?;
    let meta = live.metadata().map_err(|_| ToolError("io_denied"))?;
    let mut now = Vec::new();
    (&live)
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut now)
        .map_err(|_| ToolError("io_denied"))?;
    if &now != before {
        return Err(ToolError("changed_since_approval"));
    }
    let (dev, ino) = (meta.dev(), meta.ino());
    let mode = meta.mode() & 0o7777;
    let dot = c".";
    // SAFETY: directory fd and static C string are valid; O_TMPFILE leaves no named debris.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            dot.as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(ToolError("edit_denied"));
    }
    let mut temp = unsafe { File::from_raw_fd(fd) };
    temp.write_all(pending.request.content.as_bytes())
        .map_err(|_| ToolError("edit_denied"))?;
    // An edit must not silently change the file's permission bits.
    // SAFETY: the staged fd is live and owned here.
    unsafe {
        libc::fchmod(temp.as_raw_fd(), mode as libc::mode_t);
    }
    temp.sync_all().map_err(|_| ToolError("edit_denied"))?;
    temp.seek(SeekFrom::Start(0))
        .map_err(|_| ToolError("edit_denied"))?;
    let mut actual = Vec::new();
    (&mut temp)
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut actual)
        .map_err(|_| ToolError("edit_denied"))?;
    if actual != pending.request.content.as_bytes() {
        return Err(ToolError("verify_failed"));
    }
    Ok(Edit {
        parent,
        temp,
        name,
        dev,
        ino,
    })
}
fn commit_edit(pending: &Pending, edit: Edit) -> Result<Value, ToolError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // The name must still refer to the exact inode that was reviewed.
    if unsafe {
        libc::fstatat(
            edit.parent.as_raw_fd(),
            edit.name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(ToolError("changed_since_approval"));
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_dev != edit.dev
        || stat.st_ino != edit.ino
    {
        return Err(ToolError("changed_since_approval"));
    }
    let temp_name = cstr(&format!(".muse-edit-{}", fresh_nonce()))?;
    let empty = c"";
    // SAFETY: fds and C strings remain live; the verified inode gets a hidden
    // name and is then renamed over the reviewed name, atomically.
    let linked = unsafe {
        libc::linkat(
            edit.temp.as_raw_fd(),
            empty.as_ptr(),
            edit.parent.as_raw_fd(),
            temp_name.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    };
    if linked != 0 {
        return Err(ToolError("edit_denied"));
    }
    if unsafe {
        libc::renameat(
            edit.parent.as_raw_fd(),
            temp_name.as_ptr(),
            edit.parent.as_raw_fd(),
            edit.name.as_ptr(),
        )
    } != 0
    {
        unsafe {
            libc::unlinkat(edit.parent.as_raw_fd(), temp_name.as_ptr(), 0);
        }
        return Err(ToolError("edit_denied"));
    }
    // Read back what is now on disk: the reply describes the published file.
    let mut published = Vec::new();
    open_beneath(&pending.root, &pending.request.path, false)?
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut published)
        .map_err(|_| ToolError("verify_failed"))?;
    if published != pending.request.content.as_bytes() {
        return Err(ToolError("verify_failed"));
    }
    Ok(json!({"path":pending.request.path,"bytes":pending.request.content.len()}))
}
fn commit_rename(pending: &Pending) -> Result<Value, ToolError> {
    let target = pending
        .request
        .target_path
        .as_deref()
        .ok_or(ToolError("invalid_params"))?;
    let path = pending.request.path.as_str();
    let source = open_beneath(&pending.root, path, false)?;
    let meta = source.metadata().map_err(|_| ToolError("io_denied"))?;
    let (dev, ino) = (meta.dev(), meta.ino());
    let dest_root = pending.target_root.as_deref().unwrap_or(&pending.root);
    let from_parent = parent_of(&pending.root, path)?;
    let to_parent = parent_of(dest_root, target)?;
    let from_name = cstr(split_path(path).1)?;
    let to_name = cstr(split_path(target).1)?;
    // SAFETY: fds and C strings remain live; RENAME_NOREPLACE makes an existing
    // destination a failure rather than a silent overwrite.
    let rc = unsafe {
        libc::renameat2(
            from_parent.as_raw_fd(),
            from_name.as_ptr(),
            to_parent.as_raw_fd(),
            to_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc != 0 {
        // renameat2 between two granted roots on different filesystems
        // cannot link them; that is its own answer, not a generic denial —
        // the caller can copy via create+delete under separate approvals.
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EXDEV) {
            return Err(ToolError("cross_device"));
        }
        return Err(ToolError("rename_denied"));
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // Read-back: the destination is the reviewed inode and the old name is gone.
    if unsafe {
        libc::fstatat(
            to_parent.as_raw_fd(),
            to_name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(ToolError("verify_failed"));
    }
    let moved = unsafe { stat.assume_init() };
    if moved.st_dev != dev || moved.st_ino != ino {
        return Err(ToolError("verify_failed"));
    }
    if unsafe {
        libc::fstatat(
            from_parent.as_raw_fd(),
            from_name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err(ToolError("verify_failed"));
    }
    Ok(json!({"path":path,"new_path":target}))
}
fn commit_trash(pending: &Pending) -> Result<Value, ToolError> {
    let path = pending.request.path.as_str();
    let source = open_beneath(&pending.root, path, false)?;
    let meta = source.metadata().map_err(|_| ToolError("io_denied"))?;
    let (dev, ino) = (meta.dev(), meta.ino());
    let parent = parent_of(&pending.root, path)?;
    let name = cstr(split_path(path).1)?;
    let (files, info) = trash_directories()?;
    let base = split_path(path).1.to_string();
    let mut candidate = None;
    for attempt in 0..1000 {
        let candidate_name = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}.{attempt}")
        };
        let candidate_name_c = cstr(&candidate_name)?;
        let info_c = cstr(&format!("{candidate_name}.trashinfo"))?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let taken = unsafe {
            libc::fstatat(
                files.as_raw_fd(),
                candidate_name_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        let recorded = unsafe {
            libc::fstatat(
                info.as_raw_fd(),
                info_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        if !taken && !recorded {
            candidate = Some((candidate_name, candidate_name_c, info_c));
            break;
        }
    }
    let Some((candidate_name, candidate_name_c, info_c)) = candidate else {
        return Err(ToolError("trash_failed"));
    };
    // The .trashinfo record is written first: a move without it would be an
    // unrecoverable-looking file, so a failed write aborts before anything moves.
    // SAFETY: info dir fd, C string and mode are valid; O_EXCL prevents a race.
    let fd = unsafe {
        libc::openat(
            info.as_raw_fd(),
            info_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(ToolError("trash_failed"));
    }
    let mut record = unsafe { File::from_raw_fd(fd) };
    let body = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encode_path(&pending.root_path.join(path)),
        utc_timestamp()
    );
    if record.write_all(body.as_bytes()).is_err() || record.sync_all().is_err() {
        drop(record);
        unsafe {
            libc::unlinkat(info.as_raw_fd(), info_c.as_ptr(), 0);
        }
        return Err(ToolError("trash_failed"));
    }
    drop(record);
    // SAFETY: both fds and C strings are live; a cross-device move fails rather
    // than falling back to unlink, because Trash must stay recoverable.
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            name.as_ptr(),
            files.as_raw_fd(),
            candidate_name_c.as_ptr(),
        )
    } != 0
    {
        unsafe {
            libc::unlinkat(info.as_raw_fd(), info_c.as_ptr(), 0);
        }
        return Err(ToolError("trash_failed"));
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // Read-back: the trashed name is the reviewed inode, and the old name is gone.
    if unsafe {
        libc::fstatat(
            files.as_raw_fd(),
            candidate_name_c.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(ToolError("verify_failed"));
    }
    let trashed = unsafe { stat.assume_init() };
    if trashed.st_dev != dev || trashed.st_ino != ino {
        return Err(ToolError("verify_failed"));
    }
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err(ToolError("verify_failed"));
    }
    Ok(json!({"path":path,"trashed_as":candidate_name}))
}
/// The desktop Trash beside the user's data, created owner-only on first use.
/// Honours `XDG_DATA_HOME`, so a test can point it at a scratch directory.
fn trash_directories() -> Result<(File, File), ToolError> {
    let data_home = match std::env::var("XDG_DATA_HOME") {
        Ok(value) if value.starts_with('/') => PathBuf::from(value.trim_end_matches('/')),
        _ => match std::env::var("HOME") {
            Ok(home) if home.starts_with('/') => {
                Path::new(home.trim_end_matches('/')).join(".local/share")
            }
            _ => return Err(ToolError("trash_unavailable")),
        },
    };
    let trash = data_home.join("Trash");
    for directory in [trash.clone(), trash.join("files"), trash.join("info")] {
        if std::fs::create_dir_all(&directory).is_err() {
            return Err(ToolError("trash_unavailable"));
        }
        let _ = std::fs::set_permissions(
            &directory,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        );
    }
    let base = File::open("/").map_err(|_| ToolError("trash_unavailable"))?;
    let open = |path: &Path| -> Result<File, ToolError> {
        let relative = path
            .strip_prefix("/")
            .map_err(|_| ToolError("trash_unavailable"))?
            .to_str()
            .ok_or(ToolError("trash_unavailable"))?;
        open_beneath(&base, relative, true).map_err(|_| ToolError("trash_unavailable"))
    };
    Ok((open(&trash.join("files"))?, open(&trash.join("info"))?))
}
/// Percent-encode a path for `.trashinfo`, keeping only unreserved characters.
fn encode_path(path: &Path) -> String {
    let raw = path.as_os_str().as_encoded_bytes();
    let mut out = String::with_capacity(raw.len());
    for byte in raw {
        let byte = *byte;
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}
/// Days since the Unix epoch to a civil UTC date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
struct Dir(*mut libc::DIR);
impl Drop for Dir {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}
fn search(root: &File, query: &str, deadline: Instant) -> Result<Value, ToolError> {
    let mut stack = vec![(String::new(), 0usize)];
    let mut paths = Vec::new();
    let mut scanned = 0usize;
    while let Some((relative, depth)) = stack.pop() {
        let directory = if relative.is_empty() {
            // Open a fresh description: dup() would share readdir offsets across calls.
            let how = OpenHow {
                flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY) as u64,
                mode: 0,
                resolve: 0x08 | 0x04 | 0x02,
            };
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_openat2,
                    root.as_raw_fd(),
                    c".".as_ptr(),
                    &how,
                    std::mem::size_of::<OpenHow>(),
                )
            };
            if fd < 0 {
                return Err(ToolError("io_denied"));
            }
            unsafe { File::from_raw_fd(fd as i32) }
        } else {
            open_beneath(root, &relative, true)?
        };
        use std::os::fd::IntoRawFd;
        let fd = directory.into_raw_fd();
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(ToolError("io_denied"));
        }
        let dir = Dir(raw);
        loop {
            if Instant::now() >= deadline {
                return Err(ToolError("expired"));
            }
            unsafe {
                *libc::__errno_location() = 0;
            }
            let entry = unsafe { libc::readdir(dir.0) };
            if entry.is_null() {
                if unsafe { *libc::__errno_location() } != 0 {
                    return Err(ToolError("io_denied"));
                }
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            if scanned == 10_000 {
                return Ok(json!({"paths":paths,"scanned":scanned,"truncated":true}));
            }
            scanned += 1;
            let name_str = name.to_str().map_err(|_| ToolError("invalid_utf8"))?;
            let path = if relative.is_empty() {
                name_str.into()
            } else {
                format!("{relative}/{name_str}")
            };
            valid_path(&path)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    fd,
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(ToolError("io_denied"));
            }
            let kind = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
            match kind {
                libc::S_IFREG => {
                    if name_str.contains(query) {
                        paths.push(path);
                        if paths.len() == 100 {
                            return Ok(json!({"paths":paths,"scanned":scanned,"truncated":true}));
                        }
                    }
                }
                libc::S_IFDIR => {
                    if depth >= 64 {
                        return Err(ToolError("depth_limit"));
                    }
                    stack.push((path, depth + 1));
                }
                _ => return Err(ToolError("special_file")),
            }
        }
    }
    paths.sort();
    Ok(json!({"paths":paths,"scanned":scanned,"truncated":false}))
}
/// Content search: the same confined walk as [`search`], but a regular file
/// is opened (beneath the root, no symlinks) and its lines are matched.
/// Bounds keep one call finite: the walk's own 10,000-entry cap, at most 200
/// files opened, at most 50 matches returned, each line's text cut at 200
/// chars. Non-UTF-8 files are skipped, never errored — a folder of mixed
/// content is normal, and filename search's strictness about *names* does
/// not extend to file *bytes*.
fn search_content(root: &File, query: &str, deadline: Instant) -> Result<Value, ToolError> {
    let mut stack = vec![(String::new(), 0usize)];
    let mut matches: Vec<Value> = Vec::new();
    let mut scanned = 0usize;
    let mut files_read = 0usize;
    while let Some((relative, depth)) = stack.pop() {
        let directory = if relative.is_empty() {
            // Open a fresh description: dup() would share readdir offsets across calls.
            let how = OpenHow {
                flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY) as u64,
                mode: 0,
                resolve: 0x08 | 0x04 | 0x02,
            };
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_openat2,
                    root.as_raw_fd(),
                    c".".as_ptr(),
                    &how,
                    std::mem::size_of::<OpenHow>(),
                )
            };
            if fd < 0 {
                return Err(ToolError("io_denied"));
            }
            unsafe { File::from_raw_fd(fd as i32) }
        } else {
            open_beneath(root, &relative, true)?
        };
        use std::os::fd::IntoRawFd;
        let fd = directory.into_raw_fd();
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(ToolError("io_denied"));
        }
        let dir = Dir(raw);
        loop {
            if Instant::now() >= deadline {
                return Err(ToolError("expired"));
            }
            unsafe {
                *libc::__errno_location() = 0;
            }
            let entry = unsafe { libc::readdir(dir.0) };
            if entry.is_null() {
                if unsafe { *libc::__errno_location() } != 0 {
                    return Err(ToolError("io_denied"));
                }
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            if scanned == 10_000 {
                return Ok(json!({"matches":matches,"scanned":scanned,"truncated":true}));
            }
            scanned += 1;
            let name_str = name.to_str().map_err(|_| ToolError("invalid_utf8"))?;
            let path = if relative.is_empty() {
                name_str.into()
            } else {
                format!("{relative}/{name_str}")
            };
            valid_path(&path)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    fd,
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(ToolError("io_denied"));
            }
            let kind = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
            match kind {
                libc::S_IFREG => {
                    if files_read == 200 {
                        return Ok(json!({"matches":matches,"scanned":scanned,"truncated":true}));
                    }
                    files_read += 1;
                    let Ok(bytes) = read_bytes(root, &path) else {
                        continue;
                    };
                    let Ok(text) = std::str::from_utf8(&bytes) else {
                        continue;
                    };
                    for (index, line) in text.lines().enumerate() {
                        if line.contains(query) {
                            let excerpt: String = line.chars().take(200).collect();
                            matches.push(json!({
                                "path": path,
                                "line": index + 1,
                                "text": excerpt,
                            }));
                            if matches.len() == 50 {
                                return Ok(json!({
                                    "matches": matches,
                                    "scanned": scanned,
                                    "truncated": true
                                }));
                            }
                        }
                    }
                }
                libc::S_IFDIR => {
                    if depth >= 64 {
                        return Err(ToolError("depth_limit"));
                    }
                    stack.push((path, depth + 1));
                }
                _ => return Err(ToolError("special_file")),
            }
        }
    }
    Ok(json!({"matches":matches,"scanned":scanned,"truncated":false}))
}
impl Default for ToolBroker {
    fn default() -> Self {
        Self::new()
    }
}
fn fresh_nonce() -> String {
    let mut bytes = [0u8; 16];
    let mut filled = 0;
    while filled < bytes.len() {
        // SAFETY: remaining output buffer is writable. Entropy failure never falls back to guessable IDs.
        let result = unsafe {
            libc::getrandom(bytes[filled..].as_mut_ptr().cast(), bytes.len() - filled, 0)
        };
        if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        assert!(result > 0, "entropy_unavailable");
        filled += result as usize;
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
#[cfg(test)]
impl ToolBroker {
    fn test_pause(&self) {
        let pause = self.pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.0.wait();
            pause.1.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn granted_read_is_confined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello"), "hello").unwrap();
        let broker = ToolBroker::new();
        let grant = broker.grant(dir.path()).unwrap();
        let Decision::Reply(reply) = broker.invoke(Invocation {
            id: "read".into(),
            command: "files.read_text".into(),
            params: json!({"grant_id":grant.id,"path":"hello"}),
            timeout_ms: 1000,
        }) else {
            panic!("read must not prompt")
        };
        assert!(reply.ok);
        assert_eq!(reply.output["content"], "hello");
    }

    fn pause(b: &ToolBroker) -> Arc<(std::sync::Barrier, std::sync::Barrier)> {
        let hook = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        *b.pause.lock().unwrap() = Some(hook.clone());
        hook
    }
    #[test]
    fn revocation_discards_in_flight_read_results() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("file"), "secret").unwrap();
        let b = ToolBroker::new();
        let g = b.grant(d.path()).unwrap();
        let hook = pause(&b);
        let worker = b.clone();
        let task = std::thread::spawn(move || {
            worker.invoke(Invocation {
                id: "read".into(),
                command: "files.read_text".into(),
                params: json!({"grant_id":g.id,"path":"file"}),
                timeout_ms: 10000,
            })
        });
        hook.0.wait();
        b.revoke_all();
        hook.1.wait();
        let Decision::Reply(reply) = task.join().unwrap() else {
            panic!()
        };
        assert_eq!(reply.error.as_deref(), Some("revoked"));
        assert!(reply.output.is_null());
    }
    #[test]
    fn revocation_discards_in_flight_grant() {
        let d = tempfile::tempdir().unwrap();
        let b = ToolBroker::new();
        let hook = pause(&b);
        let worker = b.clone();
        let path = d.path().to_path_buf();
        let task = std::thread::spawn(move || worker.grant(&path));
        hook.0.wait();
        b.revoke_all();
        hook.1.wait();
        assert!(task.join().unwrap().is_err());
        assert!(b.grants().is_empty());
    }
    #[test]
    fn revocation_prevents_staged_create_commit_and_cleans_inode() {
        let d = tempfile::tempdir().unwrap();
        let b = ToolBroker::new();
        let g = b.grant(d.path()).unwrap();
        let Decision::Approval(p) = b.invoke(Invocation {
            id: "create".into(),
            command: "files.create_text".into(),
            params: json!({"grant_id":g.id,"path":"new","content":"secret"}),
            timeout_ms: 10000,
        }) else {
            panic!()
        };
        let hook = pause(&b);
        let worker = b.clone();
        let task = std::thread::spawn(move || worker.approve(&p.approval_id));
        hook.0.wait();
        b.revoke_all();
        hook.1.wait();
        let reply = task.join().unwrap();
        assert_eq!(reply.error.as_deref(), Some("revoked"));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }
}
