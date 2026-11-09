//! Host-side permissions. Model text never creates or approves a permission.
use crate::agents::{
    events::{AgentEvent, EventBus},
    model::id,
};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{oneshot, Mutex},
};
use tokio_util::sync::CancellationToken;
const FILE_LIMIT: u64 = 2 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 24000;
pub fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
fn protected(path: &Path) -> bool {
    path.components().any(|c| {
        let n = c.as_os_str().to_string_lossy().to_lowercase();
        n == ".git"
            || n == ".env"
            || n.starts_with(".env.")
            || n == ".ssh"
            || n == ".codex"
            || n.ends_with(".pem")
            || n.ends_with(".key")
    })
}
fn link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0 || meta.file_type().is_symlink()
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
/// Reject traversal, device/UNC paths, symlinks and Windows junctions, including parents.
pub fn scoped(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.contains(':')
        || relative.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        || protected(path)
    {
        bail!("Path denied: {relative}");
    }
    let mut dest = root.to_path_buf();
    for part in path.components() {
        if let Component::Normal(c) = part {
            let name = c.to_string_lossy();
            let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
            if name.ends_with(['.', ' '])
                || name.chars().any(char::is_control)
                || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (base.len() == 4
                    && (base.starts_with("COM") || base.starts_with("LPT"))
                    && base.as_bytes()[3].is_ascii_digit())
            {
                bail!("Device or ambiguous file name denied");
            }
            dest.push(c);
            if let Ok(meta) = std::fs::symlink_metadata(&dest) {
                if link(&meta) {
                    bail!("Symlink/junction denied: {}", dest.display());
                }
            }
        }
    }
    if !dest.starts_with(root) {
        bail!("Path escapes workspace");
    }
    Ok(dest)
}
pub fn portable(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolCall {
    ReadAttachment {
        id: String,
        offset: usize,
        limit: usize,
    },
    ReadFile {
        path: String,
    },
    ListDirectory {
        path: String,
    },
    SearchCode {
        pattern: String,
    },
    WriteFile {
        path: String,
        content: String,
    },
    SaveGeneratedFile {
        path: String,
        base64: String,
    },
    DeleteFile {
        path: String,
    },
    ApplyPatch {
        patch: String,
    },
    ViewDiff,
    RunCommand {
        program: String,
        args: Vec<String>,
    },
    Git {
        args: Vec<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool: String,
    pub ok: bool,
    pub output: String,
    pub exit_code: Option<i32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub before_hash: Option<String>,
    #[serde(default)]
    pub binary: bool,
}
impl FileChange {
    pub fn after_bytes(&self) -> Result<Option<Vec<u8>>> {
        self.after
            .as_ref()
            .map(|content| {
                if self.binary {
                    Ok(base64::engine::general_purpose::STANDARD.decode(content)?)
                } else {
                    Ok(content.as_bytes().to_vec())
                }
            })
            .transpose()
    }
    pub fn diff(&self) -> String {
        if self.binary {
            return format!(
                "Binary change: {}\nBefore SHA256: {:?}\nAfter bytes: {}\n",
                self.path,
                self.before_hash,
                self.after_bytes()
                    .ok()
                    .flatten()
                    .map(|b| b.len())
                    .unwrap_or(0)
            );
        }
        let old = self.before.as_deref().unwrap_or("");
        let new = self.after.as_deref().unwrap_or("");
        let mut out = format!(
            "--- {}\n+++ {}\n@@ -{},{} +{},{} @@\n",
            if self.before.is_some() {
                format!("a/{}", self.path)
            } else {
                "/dev/null".into()
            },
            if self.after.is_some() {
                format!("b/{}", self.path)
            } else {
                "/dev/null".into()
            },
            if old.is_empty() { 0 } else { 1 },
            old.lines().count(),
            if new.is_empty() { 0 } else { 1 },
            new.lines().count()
        );
        for (prefix, body) in [('-', old), ('+', new)] {
            for line in body.lines() {
                out.push(prefix);
                out.push_str(line);
                out.push('\n');
            }
            if !body.is_empty() && !body.ends_with('\n') {
                out.push_str("\\ No newline at end of file\n");
            }
        }
        out
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub agent_id: String,
    pub description: String,
    pub cwd: PathBuf,
    pub action: serde_json::Value,
}
#[derive(Clone)]
pub struct ApprovalBroker {
    mode: Arc<std::sync::atomic::AtomicU8>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    requests: Arc<Mutex<HashMap<String, ApprovalRequest>>>,
    bus: EventBus,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    #[default]
    AskForApproval,
    ApproveForMe,
    FullAccess,
}
impl AccessMode {
    pub fn allows(self, action: &serde_json::Value) -> bool {
        if self == Self::FullAccess {
            return true;
        }
        if self == Self::AskForApproval {
            return false;
        }
        match action["tool"].as_str().unwrap_or("") {
            "apply_patch" | "delete_file" => true, // Validated operations in a private copy.
            "run_command" => {
                let Some(args) = action["args"].as_array() else {
                    return false;
                };
                action["program"] == "cargo"
                    && args
                        .first()
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| matches!(s, "check" | "build" | "test"))
                    && args.iter().all(|v| {
                        matches!(
                            v.as_str(),
                            Some(
                                "check"
                                    | "build"
                                    | "test"
                                    | "--locked"
                                    | "--offline"
                                    | "--release"
                                    | "--all-targets"
                                    | "--all-features"
                                    | "--no-default-features"
                                    | "--workspace"
                            )
                        )
                    })
            }
            _ => false,
        }
    }
}
impl ApprovalBroker {
    pub fn set_mode(&self, mode: AccessMode) {
        self.mode
            .store(mode as u8, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn new(bus: EventBus) -> Self {
        Self {
            mode: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            pending: Default::default(),
            requests: Default::default(),
            bus,
        }
    }
    pub async fn request(
        &self,
        agent_id: &str,
        description: String,
        cwd: PathBuf,
        action: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let mode = match self.mode.load(std::sync::atomic::Ordering::SeqCst) {
            1 => AccessMode::ApproveForMe,
            2 => AccessMode::FullAccess,
            _ => AccessMode::AskForApproval,
        };
        if mode.allows(&action) {
            self.bus.publish(AgentEvent::Activity {
                agent_id: agent_id.into(),
                role: "policy".into(),
                content: format!(
                    "Automatically allowed by selected {mode:?} policy: {}",
                    truncate(&serde_json::to_string(&action)?, 8000)
                ),
            });
            return Ok(true);
        }
        let key = id("approval");
        let (tx, rx) = oneshot::channel();
        let request = ApprovalRequest {
            id: key.clone(),
            agent_id: agent_id.into(),
            description,
            cwd,
            action,
        };
        self.pending.lock().await.insert(key.clone(), tx);
        self.requests
            .lock()
            .await
            .insert(key.clone(), request.clone());
        self.bus.publish(AgentEvent::Approval(request));
        let result = tokio::select! {_ = cancel.cancelled()=>false,value=tokio::time::timeout(Duration::from_secs(900),rx)=>value.ok().and_then(|x|x.ok()).unwrap_or(false)};
        self.pending.lock().await.remove(&key);
        self.requests.lock().await.remove(&key);
        self.bus.publish(AgentEvent::ApprovalResolved {
            id: key,
            allowed: result,
        });
        Ok(result)
    }
    pub async fn decide(&self, id: &str, allowed: bool) -> bool {
        if let Some(sender) = self.pending.lock().await.remove(id) {
            let _ = sender.send(allowed);
            true
        } else {
            false
        }
    }
    pub async fn pending(&self) -> Vec<ApprovalRequest> {
        self.requests.lock().await.values().cloned().collect()
    }
}
#[derive(Clone)]
pub struct WorkspaceSnapshot {
    pub source: PathBuf,
    pub files: Arc<BTreeMap<String, Vec<u8>>>,
}
impl WorkspaceSnapshot {
    pub async fn capture(source: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            let source = source
                .canonicalize()
                .context("Selected workspace does not exist")?;
            if !source.is_dir() {
                bail!("Workspace must be a directory");
            }
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&source)
                .args([
                    "-c",
                    "core.fsmonitor=false",
                    "-c",
                    "core.hooksPath=NUL",
                    "--no-pager",
                    "ls-files",
                    "-z",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                ])
                .output();
            let names: Vec<String> = if let Ok(output) = output {
                if output.status.success() {
                    output
                        .stdout
                        .split(|b| *b == 0)
                        .filter(|b| !b.is_empty())
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                        .collect()
                } else {
                    walk(&source)?
                }
            } else {
                walk(&source)?
            };
            let mut files = BTreeMap::new();
            let mut total = 0u64;
            for name in names {
                if skip(&name) || protected(Path::new(&name)) {
                    continue;
                }
                let Ok(path) = scoped(&source, &name) else {
                    continue;
                };
                let meta = match std::fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                if !meta.is_file() {
                    continue;
                }
                if meta.len() > FILE_LIMIT {
                    continue;
                }
                total += meta.len();
                if total > 64 * 1024 * 1024 || files.len() >= 5000 {
                    bail!(
                        "Workspace snapshot exceeds 64 MiB or 5000 files; select a smaller project"
                    );
                }
                files.insert(name, std::fs::read(path)?);
            }
            Ok(Self {
                source,
                files: Arc::new(files),
            })
        })
        .await?
    }
    pub async fn isolate(&self, path: PathBuf, overlay: &[FileChange]) -> Result<ToolExecutor> {
        let snapshot = self.clone();
        let changes = overlay.to_vec();
        tokio::task::spawn_blocking(move || {
            if path.exists() {
                bail!("Isolation directory already exists");
            }
            std::fs::create_dir_all(&path)?;
            let path = path.canonicalize()?;
            for (name, bytes) in snapshot.files.iter() {
                let dest = scoped(&path, name)?;
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(dest, bytes)?;
            }
            for change in changes {
                let dest = scoped(&path, &change.path)?;
                match change.after_bytes()? {
                    Some(bytes) => {
                        if let Some(p) = dest.parent() {
                            std::fs::create_dir_all(p)?;
                        }
                        std::fs::write(dest, bytes)?;
                    }
                    None => {
                        if dest.exists() {
                            std::fs::remove_file(dest)?;
                        }
                    }
                }
            }
            // A private Git repository makes diffs/status available without sharing the source .git.
            let empty = path.join(".agent-empty-hooks");
            std::fs::create_dir_all(&empty)?;
            let init = std::process::Command::new("git")
                .arg("-c")
                .arg(format!("init.templateDir={}", empty.display()))
                .arg("init")
                .arg("--quiet")
                .arg(&path)
                .output()?;
            if !init.status.success() {
                bail!("Could not initialize isolated Git repository");
            }
            Ok(ToolExecutor {
                root: path,
                snapshot,
                approval: None,
                agent_id: String::new(),
                attachments: vec![],
            })
        })
        .await?
    }
}
fn skip(name: &str) -> bool {
    name.split(['/', '\\']).any(|part| {
        matches!(
            part,
            "target"
                | "node_modules"
                | ".git"
                | ".idea"
                | "webview_data"
                | "profile_gemini"
                | "profile_ai_mode"
                | ".agent-empty-hooks"
                | ".agent-patches"
        )
    })
}
fn walk(root: &Path) -> Result<Vec<String>> {
    fn visit(root: &Path, dir: &Path, files: &mut Vec<String>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let meta = std::fs::symlink_metadata(entry.path())?;
            let name = portable(entry.path().strip_prefix(root)?);
            if link(&meta) || skip(&name) || protected(Path::new(&name)) {
                continue;
            }
            if meta.is_dir() {
                visit(root, &entry.path(), files)?;
            } else if meta.is_file() {
                files.push(name);
            }
            if files.len() > 5000 {
                bail!("Too many workspace files");
            }
        }
        Ok(())
    }
    let mut files = vec![];
    visit(root, root, &mut files)?;
    Ok(files)
}
#[derive(Clone)]
pub struct ToolExecutor {
    pub attachments: Vec<crate::attachments::Attachment>,
    pub root: PathBuf,
    pub snapshot: WorkspaceSnapshot,
    pub approval: Option<ApprovalBroker>,
    pub agent_id: String,
}
impl ToolExecutor {
    pub async fn changes(&self) -> Result<Vec<FileChange>> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut paths: Vec<String> = this.snapshot.files.keys().cloned().collect();
            paths.extend(walk(&this.root)?);
            paths.sort();
            paths.dedup();
            let mut changes = vec![];
            let mut change_bytes = 0usize;
            for name in paths {
                if skip(&name) {
                    continue;
                }
                let path = scoped(&this.root, &name)?;
                let before = this.snapshot.files.get(&name);
                let after = if path.is_file() {
                    if std::fs::metadata(&path)?.len() > FILE_LIMIT {
                        bail!("Modified file exceeds limit: {name}");
                    }
                    Some(std::fs::read(&path)?)
                } else {
                    None
                };
                if before.map(Vec::as_slice) == after.as_deref() {
                    continue;
                }
                let binary = before.is_some_and(|b| std::str::from_utf8(b).is_err())
                    || after
                        .as_ref()
                        .is_some_and(|b| std::str::from_utf8(b).is_err());
                let encode = |b: &Vec<u8>| {
                    if binary {
                        base64::engine::general_purpose::STANDARD.encode(b)
                    } else {
                        String::from_utf8_lossy(b).into_owned()
                    }
                };
                let old = before.map(encode);
                let new = after.as_ref().map(encode);
                change_bytes += old.as_ref().map(String::len).unwrap_or(0)
                    + new.as_ref().map(String::len).unwrap_or(0);
                if changes.len() >= 100 || change_bytes > 16 * 1024 * 1024 {
                    bail!("Task change set exceeds 100 files or 16 MiB; split the request");
                }
                changes.push(FileChange {
                    path: name,
                    before_hash: before.map(|b| digest(b)),
                    before: old,
                    after: new,
                    binary,
                });
            }
            Ok(changes)
        })
        .await?
    }
    pub async fn execute(&self, call: ToolCall, cancel: &CancellationToken) -> ToolResult {
        let name = serde_json::to_value(&call).unwrap()["tool"]
            .as_str()
            .unwrap()
            .to_owned();
        let result = self.execute_inner(&call, cancel).await;
        match result {
            Ok((output, exit_code)) => ToolResult {
                tool: name,
                ok: exit_code.is_none_or(|c| c == 0),
                output,
                exit_code,
            },
            Err(error) => ToolResult {
                tool: name,
                ok: false,
                output: format!("{error:#}"),
                exit_code: None,
            },
        }
    }
    async fn execute_inner(
        &self,
        call: &ToolCall,
        cancel: &CancellationToken,
    ) -> Result<(String, Option<i32>)> {
        if cancel.is_cancelled() {
            bail!("Cancelled");
        }
        match call {
            ToolCall::ReadAttachment { id, offset, limit } => {
                let attachment = self
                    .attachments
                    .iter()
                    .find(|a| &a.id == id)
                    .context("Attachment was not selected by the user")?;
                if attachment.native_upload {
                    bail!("Binary attachment requires the page's native upload capability");
                }
                let bytes = tokio::fs::read(&attachment.stored_path).await?;
                if digest(&bytes) != attachment.sha256 {
                    bail!("Attachment changed after selection");
                }
                let text = std::str::from_utf8(&bytes)?;
                if *offset > text.len() || !text.is_char_boundary(*offset) {
                    bail!("Invalid UTF-8 byte offset");
                }
                let mut end = offset
                    .saturating_add((*limit).clamp(4, 16000))
                    .min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                Ok((serde_json::json!({"text":&text[*offset..end],"next_offset":end,"total_bytes":text.len(),"eof":end==text.len()}).to_string(),None))
            }
            ToolCall::ReadFile { path } => {
                let path = scoped(&self.root, path)?;
                if tokio::fs::metadata(&path).await?.len() > FILE_LIMIT {
                    bail!("File too large");
                }
                let text = tokio::fs::read_to_string(path).await?;
                Ok((truncate(&text, OUTPUT_LIMIT), None))
            }
            ToolCall::ListDirectory { path } => {
                let path = scoped(&self.root, if path.is_empty() { "." } else { path })?;
                let mut entries = tokio::fs::read_dir(path).await?;
                let mut out = vec![];
                while let Some(entry) = entries.next_entry().await? {
                    let n = entry.file_name().to_string_lossy().into_owned();
                    if !protected(Path::new(&n)) && n != ".git" {
                        out.push(format!(
                            "{}{}",
                            n,
                            if entry.file_type().await?.is_dir() {
                                "/"
                            } else {
                                ""
                            }
                        ));
                    }
                    if out.len() >= 400 {
                        break;
                    }
                }
                out.sort();
                Ok((out.join("\n"), None))
            }
            ToolCall::SearchCode { pattern } => {
                if pattern.len() > 500 {
                    bail!("Search pattern too long");
                }
                process(
                    &self.root,
                    "rg",
                    &[
                        "--fixed-strings".into(),
                        "--line-number".into(),
                        "--max-count".into(),
                        "30".into(),
                        "--glob".into(),
                        "!target/**".into(),
                        "--glob".into(),
                        "!.git/**".into(),
                        "--".into(),
                        pattern.clone(),
                        ".".into(),
                    ],
                    cancel,
                    30,
                )
                .await
            }
            ToolCall::WriteFile { path, content } => {
                if content.len() as u64 > FILE_LIMIT {
                    bail!("File too large");
                }
                let path = scoped(&self.root, path)?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let path = scoped(&self.root, &portable(path.strip_prefix(&self.root)?))?;
                tokio::fs::write(path, content).await?;
                Ok((
                    "Written in isolated working copy only; source application requires approval."
                        .into(),
                    None,
                ))
            }
            ToolCall::SaveGeneratedFile {
                path,
                base64: content,
            } => {
                if content.len() > FILE_LIMIT as usize * 2 {
                    bail!("Generated file exceeds size limit");
                }
                let bytes = base64::engine::general_purpose::STANDARD.decode(content)?;
                if bytes.len() as u64 > FILE_LIMIT {
                    bail!("Generated file exceeds size limit");
                }
                let path = scoped(&self.root, path)?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(path, bytes).await?;
                Ok(("Generated file staged for approval".into(), None))
            }
            ToolCall::DeleteFile { path } => {
                let dest = scoped(&self.root, path)?;
                if !dest.is_file() {
                    bail!("Only an existing regular file can be deleted");
                }
                let broker = self
                    .approval
                    .as_ref()
                    .context("Deletion requires approval")?;
                if !broker.request(&self.agent_id,"Delete this file from the isolated copy? Source deletion will require a separate final approval.".into(),self.root.clone(),serde_json::to_value(call)?,cancel).await?{bail!("Deletion denied");}
                let dest = scoped(&self.root, path)?;
                tokio::fs::remove_file(dest).await?;
                Ok(("Deleted from isolated copy; source unchanged".into(), None))
            }
            ToolCall::ApplyPatch { patch } => {
                validate_patch(&self.root, patch)?;
                let broker = self
                    .approval
                    .as_ref()
                    .context("Patch application requires approval")?;
                if !broker.request(&self.agent_id,"Apply this exact unified diff in the isolated copy? The selected source project remains unchanged.".into(),self.root.clone(),serde_json::to_value(call)?,cancel).await?{bail!("Patch denied");}
                let folder = self.root.join(".agent-patches");
                tokio::fs::create_dir_all(&folder).await?;
                let path = folder.join(format!("{}.patch", id("patch")));
                tokio::fs::write(&path, patch).await?;
                let args = vec![
                    "-c".into(),
                    "core.fsmonitor=false".into(),
                    "apply".into(),
                    "--check".into(),
                    portable(path.strip_prefix(&self.root)?),
                ];
                let check = process(&self.root, "git", &args, cancel, 30).await?;
                if check.1 != Some(0) {
                    return Ok(check);
                }
                let args = vec![
                    "-c".into(),
                    "core.fsmonitor=false".into(),
                    "apply".into(),
                    portable(path.strip_prefix(&self.root)?),
                ];
                process(&self.root, "git", &args, cancel, 30).await
            }
            ToolCall::ViewDiff => {
                let changes = self.changes().await?;
                Ok((
                    truncate(
                        &changes
                            .iter()
                            .map(FileChange::diff)
                            .collect::<Vec<_>>()
                            .join("\n"),
                        OUTPUT_LIMIT,
                    ),
                    None,
                ))
            }
            ToolCall::RunCommand { program, args } => {
                self.approved_process(program, args, call, cancel).await
            }
            ToolCall::Git { args } => self.approved_process("git", args, call, cancel).await,
        }
    }
    async fn approved_process(
        &self,
        program: &str,
        args: &[String],
        call: &ToolCall,
        cancel: &CancellationToken,
    ) -> Result<(String, Option<i32>)> {
        if program.is_empty() || args.len() > 100 {
            bail!("Invalid command");
        }
        let broker = self
            .approval
            .as_ref()
            .context("Command execution requires an interactive approval broker")?;
        let allowed=broker.request(&self.agent_id,"Run this exact command on the host in the isolated copy? Project code can read/write outside the workspace; approval authorizes this host command. This is not an OS sandbox.".into(),self.root.clone(),serde_json::to_value(call)?,cancel).await?;
        if !allowed {
            bail!("Command denied or approval expired");
        }
        process(&self.root, program, args, cancel, 180).await
    }
}
pub fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.into();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output truncated]", &value[..end])
}
async fn drain<R: tokio::io::AsyncRead + Unpin>(mut reader: R) -> Vec<u8> {
    let mut kept = vec![];
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let available = OUTPUT_LIMIT.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(available)]);
            }
        }
    }
    kept
}
pub async fn process(
    root: &Path,
    program: &str,
    args: &[String],
    cancel: &CancellationToken,
    seconds: u64,
) -> Result<(String, Option<i32>)> {
    use std::process::Stdio;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("Could not start {program}"))?;
    let pid = child.id();
    let out = tokio::spawn(drain(child.stdout.take().unwrap()));
    let err = tokio::spawn(drain(child.stderr.take().unwrap()));
    let status = tokio::select! {_ = cancel.cancelled()=>None,result=tokio::time::timeout(Duration::from_secs(seconds),child.wait())=>match result{Ok(status)=>Some(status?),Err(_)=>None}};
    if status.is_none() {
        #[cfg(windows)]
        if let Some(pid) = pid {
            let _ = tokio::process::Command::new("taskkill.exe")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(0x08000000)
                .output()
                .await;
        }
        #[cfg(not(windows))]
        let _ = pid;
        let _ = child.kill().await;
        out.abort();
        err.abort();
        bail!("Command cancelled or timed out; process terminated");
    }
    let output = tokio::time::timeout(Duration::from_secs(2), async {
        Ok::<_, anyhow::Error>((out.await?, err.await?))
    })
    .await
    .context("Command descendants kept output pipes open")??;
    Ok((
        format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.0),
            String::from_utf8_lossy(&output.1)
        ),
        status.unwrap().code(),
    ))
}
/// Apply only a previously reviewed immutable change set. Optimistic hashes protect user edits.
pub fn validate_patch(root: &Path, patch: &str) -> Result<()> {
    if patch.len() > 1024 * 1024 {
        bail!("Patch exceeds 1 MiB");
    }
    let mut headers = 0;
    let mut hunks = 0;
    // Remaining old/new lines of the current hunk. Inside a hunk, a removed
    // "-- comment" line reads as "--- comment" and is content, not a header.
    let (mut old_left, mut new_left) = (0usize, 0usize);
    for line in patch.lines() {
        if old_left > 0 || new_left > 0 {
            match line.as_bytes().first() {
                Some(b' ') | None => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
                Some(b'-') => old_left = old_left.saturating_sub(1),
                Some(b'+') => new_left = new_left.saturating_sub(1),
                Some(b'\\') => {}
                _ => bail!("Malformed hunk body"),
            }
            continue;
        }
        if let Some(range) = line.strip_prefix("@@ ") {
            (old_left, new_left) = hunk_counts(range)?;
            hunks += 1;
            continue;
        }
        if line.starts_with("rename ")
            || line.starts_with("copy ")
            || line.contains("mode 120000")
            || line.starts_with("GIT binary patch")
        {
            bail!("Renames, symlink modes and binary Git patches are unsupported; use explicit file tools");
        }
        if let Some(header) = line
            .strip_prefix("--- ")
            .or_else(|| line.strip_prefix("+++ "))
        {
            headers += 1;
            if header == "/dev/null" {
                continue;
            }
            let path = header
                .strip_prefix("a/")
                .or_else(|| header.strip_prefix("b/"))
                .ok_or_else(|| anyhow::anyhow!("Patch paths must use a/ and b/ prefixes"))?;
            if path.chars().any(char::is_whitespace) || path.contains('"') {
                bail!("Use write_file for quoted or whitespace-containing patch paths");
            }
            scoped(root, path)?;
        }
        if let Some(paths) = line.strip_prefix("diff --git ") {
            let paths: Vec<_> = paths.split_whitespace().collect();
            if paths.len() != 2 {
                bail!("Unsupported diff header");
            }
            for path in paths {
                let path = path
                    .strip_prefix("a/")
                    .or_else(|| path.strip_prefix("b/"))
                    .ok_or_else(|| anyhow::anyhow!("Invalid Git patch path"))?;
                scoped(root, path)?;
            }
        }
    }
    if headers < 2 || headers % 2 != 0 || hunks == 0 {
        bail!("A complete unified patch with file headers and hunks is required");
    }
    Ok(())
}
/// Line counts from a hunk header body such as "-1,3 +1,4 @@".
fn hunk_counts(range: &str) -> Result<(usize, usize)> {
    let mut parts = range.split_whitespace();
    let mut count = |sign: char| -> Result<usize> {
        let part = parts
            .next()
            .and_then(|p| p.strip_prefix(sign))
            .context("Malformed hunk header")?;
        Ok(match part.split_once(',') {
            Some((_, n)) => n.parse()?,
            None => {
                part.parse::<usize>()?;
                1
            }
        })
    };
    Ok((count('-')?, count('+')?))
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_file_name(format!(".evil-{}.tmp", id("write")));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
pub fn apply_changes(root: &Path, changes: &[FileChange]) -> Result<()> {
    let root = root.canonicalize()?;
    let mut backups = vec![];
    let mut paths = std::collections::HashSet::new();
    for change in changes {
        if !paths.insert(&change.path) {
            bail!("Duplicate source path in change set");
        }
        let path = scoped(&root, &change.path)?;
        let old = if path.exists() {
            Some(std::fs::read(&path)?)
        } else {
            None
        };
        if old.as_deref().map(digest) != change.before_hash {
            bail!("Source changed since review: {}", change.path);
        }
        let bytes = change.after_bytes()?;
        if bytes.as_ref().is_some_and(|b| b.len() as u64 > FILE_LIMIT) {
            bail!("Source change exceeds file limit");
        }
        backups.push((path, old, bytes));
    }
    let mut applied = 0;
    for (index, change) in changes.iter().enumerate() {
        let result = (|| -> Result<()> {
            let path = scoped(&root, &change.path)?;
            let current = if path.exists() {
                Some(std::fs::read(&path)?)
            } else {
                None
            };
            if current.as_deref().map(digest) != change.before_hash {
                bail!("Source changed while applying: {}", change.path);
            }
            match &backups[index].2 {
                Some(bytes) => atomic_write(&path, bytes)?,
                None => {
                    if path.exists() {
                        std::fs::remove_file(&path)?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            for (path, old, expected) in backups[..applied].iter().rev() {
                let current = std::fs::read(path).ok();
                if current.as_deref().map(digest) != expected.as_deref().map(digest) {
                    continue;
                }
                match old {
                    Some(bytes) => {
                        let _ = atomic_write(path, bytes);
                    }
                    None => {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            return Err(error);
        }
        applied += 1;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approval_modes_have_host_enforced_distinct_permissions() {
        let build =
            serde_json::json!({"tool":"run_command","program":"cargo","args":["test","--locked"]});
        assert!(!AccessMode::AskForApproval.allows(&build));
        assert!(AccessMode::ApproveForMe.allows(&build));
        for action in [
            serde_json::json!({"tool":"run_command","program":"cmd","args":["/c","del"]}),
            serde_json::json!({"tool":"run_command","program":"cargo","args":["clean"]}),
            serde_json::json!({"tool":"run_command","program":"cargo","args":["test","--target-dir","C:/outside"]}),
            serde_json::json!({"tool":"apply_changes","changes":[]}),
        ] {
            assert!(!AccessMode::ApproveForMe.allows(&action));
            assert!(AccessMode::FullAccess.allows(&action));
        }
        assert!(AccessMode::ApproveForMe
            .allows(&serde_json::json!({"tool":"apply_patch","patch":"validated separately"})));
    }
    #[tokio::test]
    async fn selected_full_access_is_applied_by_the_broker_without_fake_ui_approval() {
        let broker = ApprovalBroker::new(EventBus::default());
        broker.set_mode(AccessMode::FullAccess);
        let result = broker
            .request(
                "agent",
                "Explicitly enabled full access".into(),
                PathBuf::from("C:/public"),
                serde_json::json!({"tool":"run_command","program":"cargo","args":["test"]}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result);
        assert!(broker.pending().await.is_empty());
        let defaults: crate::agents::AgentPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.access_mode, AccessMode::AskForApproval);
    }
    #[test]
    fn traversal_and_sensitive_paths_are_denied() {
        let root = tempfile::tempdir().unwrap();
        for p in [
            "../outside",
            "C:/outside",
            ".git/config",
            ".env",
            "folder/../../x",
            "NUL.txt",
            "folder/CON",
            "a.rs.",
            "a.rs ",
        ] {
            assert!(scoped(root.path(), p).is_err(), "{p}");
        }
    }
    #[test]
    fn hunk_lines_resembling_headers_are_content() {
        let root = tempfile::tempdir().unwrap();
        let patch = "--- a/q.sql\n+++ b/q.sql\n@@ -1,2 +1,2 @@\n--- old comment\n+++ counter\n select 1;\n";
        validate_patch(root.path(), patch).unwrap();
        let removed = "--- a/q.sql\n+++ b/q.sql\n@@ -1 +0,0 @@\n--- note\n";
        validate_patch(root.path(), removed).unwrap();
        assert!(validate_patch(root.path(), "--- a/x\n+++ b/x\n@@ bad @@\n").is_err());
    }
    #[test]
    fn stale_changes_cannot_overwrite_user_edits() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.rs"), "new user edit").unwrap();
        let c = FileChange {
            path: "a.rs".into(),
            before: Some("old".into()),
            after: Some("agent".into()),
            before_hash: Some(digest(b"old")),
            binary: false,
        };
        assert!(apply_changes(root.path(), &[c]).is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("a.rs")).unwrap(),
            "new user edit"
        );
    }
    #[test]
    fn binary_apply_and_duplicate_paths_are_checked() {
        let root = tempfile::tempdir().unwrap();
        let old = vec![0, 255, 1];
        let new = vec![0, 254, 2];
        std::fs::write(root.path().join("image.bin"), &old).unwrap();
        let change = FileChange {
            path: "image.bin".into(),
            before: None,
            after: Some(base64::engine::general_purpose::STANDARD.encode(&new)),
            before_hash: Some(digest(&old)),
            binary: true,
        };
        assert!(apply_changes(root.path(), &[change.clone(), change.clone()]).is_err());
        assert_eq!(std::fs::read(root.path().join("image.bin")).unwrap(), old);
        apply_changes(root.path(), &[change]).unwrap();
        assert_eq!(std::fs::read(root.path().join("image.bin")).unwrap(), new);
    }
    #[tokio::test]
    async fn attachments_can_be_read_beyond_preview_without_accepting_arbitrary_paths() {
        let source = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let file = storage.path().join("long.txt");
        std::fs::write(&file, format!("{}TAIL", "я".repeat(9000))).unwrap();
        let attachment =
            crate::attachments::Attachment::prepare(file, storage.path().join("files"), 20000)
                .await
                .unwrap();
        let snapshot = WorkspaceSnapshot::capture(source.path().into())
            .await
            .unwrap();
        let mut tools = snapshot
            .isolate(storage.path().join("isolated"), &[])
            .await
            .unwrap();
        tools.attachments = vec![attachment.clone()];
        let cancel = CancellationToken::new();
        let mut offset = 0;
        let mut full = String::new();
        loop {
            let result = tools
                .execute(
                    ToolCall::ReadAttachment {
                        id: attachment.id.clone(),
                        offset,
                        limit: 15001,
                    },
                    &cancel,
                )
                .await;
            assert!(result.ok, "{}", result.output);
            let data: serde_json::Value = serde_json::from_str(&result.output).unwrap();
            full.push_str(data["text"].as_str().unwrap());
            offset = data["next_offset"].as_u64().unwrap() as usize;
            if data["eof"] == true {
                break;
            }
        }
        assert!(full.ends_with("TAIL"));
        assert_eq!(full.len(), 18004);
        assert!(
            !tools
                .execute(
                    ToolCall::ReadAttachment {
                        id: "C:/private.txt".into(),
                        offset: 0,
                        limit: 100
                    },
                    &cancel
                )
                .await
                .ok
        );
    }
    #[tokio::test]
    async fn approved_patch_and_compiler_errors_are_real_tool_results() {
        let source = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("main.rs"), "fn main() {}\n").unwrap();
        let snapshot = WorkspaceSnapshot::capture(source.path().into())
            .await
            .unwrap();
        let mut tools = snapshot
            .isolate(storage.path().join("isolated"), &[])
            .await
            .unwrap();
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let broker = ApprovalBroker::new(bus);
        tools.approval = Some(broker.clone());
        tools.agent_id = "test".into();
        let responder = tokio::spawn(async move {
            let mut count = 0;
            while count < 2 {
                if let AgentEvent::Approval(r) = events.recv().await.unwrap() {
                    broker.decide(&r.id, true).await;
                    count += 1;
                }
            }
        });
        let cancel = CancellationToken::new();
        let patch =
            "--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-fn main() {}\n+fn main() { missing(); }\n";
        let result = tools
            .execute(
                ToolCall::ApplyPatch {
                    patch: patch.into(),
                },
                &cancel,
            )
            .await;
        assert!(result.ok, "{}", result.output);
        let result = tools
            .execute(
                ToolCall::RunCommand {
                    program: "rustc".into(),
                    args: vec!["main.rs".into()],
                },
                &cancel,
            )
            .await;
        assert!(!result.ok);
        assert!(result.output.contains("E0425"), "{}", result.output);
        responder.await.unwrap();
        assert_eq!(
            std::fs::read_to_string(source.path().join("main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(validate_patch(
            &tools.root,
            "--- a/../outside\n+++ b/../outside\n@@ -1 +1 @@\n-x\n+y\n"
        )
        .is_err());
    }
    #[tokio::test]
    async fn denied_command_does_not_execute() {
        let bus = EventBus::default();
        let broker = ApprovalBroker::new(bus.clone());
        let mut events = bus.subscribe();
        let other = broker.clone();
        let cancel = CancellationToken::new();
        let pending = tokio::spawn(async move {
            other
                .request(
                    "a",
                    "test".into(),
                    PathBuf::new(),
                    serde_json::json!({"program":"danger"}),
                    &cancel,
                )
                .await
                .unwrap()
        });
        if let AgentEvent::Approval(request) = events.recv().await.unwrap() {
            assert!(broker.decide(&request.id, false).await);
        }
        assert!(!pending.await.unwrap());
    }
}
