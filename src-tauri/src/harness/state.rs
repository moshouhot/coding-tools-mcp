use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use super::model::{
    BaselineEntry, CapabilityStatus, FileChangeRecord, HarnessEvent, HarnessStatus, OperationRecord,
    ProjectBaseline, ProjectFileState, ProjectState, TaskSession,
    TaskStatus, WorkspaceHarnessState, SCHEMA_VERSION,
};
use super::store::{HarnessError, HarnessResult, HarnessStore};

#[derive(Debug, Clone)]
pub struct Harness {
    workspace_root: PathBuf,
    workspace_id: String,
    store: HarnessStore,
    ignored_paths: Vec<PathBuf>,
}

impl Harness {
    pub fn new(workspace_root: PathBuf, harness_root: PathBuf) -> HarnessResult<Self> {
        Self::new_with_ignored_paths(workspace_root, harness_root, Vec::new())
    }

    pub fn new_with_ignored_paths(
        workspace_root: PathBuf,
        harness_root: PathBuf,
        mut ignored_paths: Vec<PathBuf>,
    ) -> HarnessResult<Self> {
        let workspace_root = workspace_root
            .canonicalize()
            .map_err(|e| HarnessError::new("WORKSPACE_UNAVAILABLE", e.to_string()))?;
        let default_history = workspace_root.join("docs").join("history-session");
        if !ignored_paths.iter().any(|path| path == &default_history) {
            ignored_paths.push(default_history);
        }
        let workspace_id = workspace_id(&workspace_root);
        Ok(Self {
            workspace_root,
            workspace_id,
            store: HarnessStore::new(harness_root)?,
            ignored_paths,
        })
    }

    pub fn default_root() -> HarnessResult<PathBuf> {
        let root = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| HarnessError::new("STORE_UNAVAILABLE", "无法确定应用数据目录"))?;
        Ok(root.join("coding-tools-mcp").join("harness"))
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn store_root(&self) -> &Path {
        self.store.root()
    }

    pub fn start_task(&self, objective: &str) -> HarnessResult<TaskSession> {
        if objective.trim().is_empty() {
            return Err(HarnessError::new("INVALID_ARGUMENT", "任务目标不能为空"));
        }
        if let Some(task) = self.current_task()? {
            return Err(HarnessError::new(
                "TASK_ALREADY_ACTIVE",
                format!("工作区已有活动任务 {}", task.id),
            ));
        }
        let baseline = self.capture_baseline();
        let now = timestamp();
        let task = TaskSession {
            id: Uuid::new_v4().simple().to_string(),
            workspace_id: self.workspace_id.clone(),
            objective: objective.trim().to_string(),
            status: TaskStatus::Active,
            expected_fingerprint: baseline.worktree_fingerprint.clone(),
            baseline,
            completed_steps: Vec::new(),
            pending_steps: Vec::new(),
            latest_change_id: None,
            latest_verification_id: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.save_task(&task)?;
        self.save_workspace_state(Some(&task.id), &task.updated_at)?;
        self.record_event(
            &task.id,
            "task_started",
            None,
            json!({}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn current_task(&self) -> HarnessResult<Option<TaskSession>> {
        Ok(self
            .store
            .list_tasks(&self.workspace_id)?
            .into_iter()
            .find(|task| task.status.is_writable()))
    }

    pub fn task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.store.load_task(&self.workspace_id, task_id)
    }

    pub fn transition(&self, task_id: &str, next: TaskStatus) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        if !task.status.can_transition_to(next) {
            return Err(HarnessError::new(
                "INVALID_TASK_TRANSITION",
                format!("不允许从 {:?} 转换到 {:?}", task.status, next),
            ));
        }
        task.status = next;
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        if !task.status.is_writable() {
            self.save_workspace_state(None, &task.updated_at)?;
        }
        self.record_event(
            task_id,
            "task_status_changed",
            None,
            json!({"status": next}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn update_steps(
        &self,
        task_id: &str,
        completed_steps: Option<Vec<String>>,
        pending_steps: Option<Vec<String>>,
    ) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        if let Some(steps) = completed_steps {
            task.completed_steps = steps;
        }
        if let Some(steps) = pending_steps {
            task.pending_steps = steps;
        }
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        self.record_event(
            task_id,
            "task_updated",
            None,
            json!({
                "completed_steps": task.completed_steps,
                "pending_steps": task.pending_steps
            }),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn check_baseline(&self, task_id: &str) -> HarnessResult<()> {
        let task = self.task(task_id)?;
        let current = self.capture_baseline();
        if current.branch != task.baseline.branch || current.head != task.baseline.head {
            return Err(HarnessError::new(
                "BASELINE_STALE",
                "Git 分支或 HEAD 已发生变化",
            ));
        }
        if current.worktree_fingerprint != task.expected_fingerprint {
            return Err(HarnessError::new(
                "FILE_CHANGED_EXTERNALLY",
                "工作区存在 Harness 未记录的外部文件变化",
            ));
        }
        Ok(())
    }

    pub fn refresh_expected_state(&self, task_id: &str) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        task.expected_fingerprint = self.capture_baseline().worktree_fingerprint;
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn record_event(
        &self,
        task_id: &str,
        kind: &str,
        tool_name: Option<&str>,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<HarnessEvent> {
        let event = HarnessEvent {
            id: Uuid::new_v4().simple().to_string(),
            task_id: task_id.to_string(),
            operation_id: Uuid::new_v4().simple().to_string(),
            kind: kind.to_string(),
            tool_name: tool_name.map(str::to_string),
            input_summary: json!({"workspace_id": self.workspace_id, "payload": input_summary}),
            result_summary,
            reason: None,
            affected_files: Vec::<FileChangeRecord>::new(),
            created_at: timestamp(),
        };
        self.store
            .append_event_for_workspace(&self.workspace_id, &event)?;
        Ok(event)
    }

    pub fn list_events(
        &self,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        self.store
            .list_events(&self.workspace_id, task_id, offset, limit)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_operation(
        &self,
        operation_id: Option<&str>,
        task_id: Option<&str>,
        tool: &str,
        kind: &str,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<OperationRecord> {
        let reason = input_summary
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let operation = OperationRecord {
            id: operation_id
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
            workspace_id: self.workspace_id.clone(),
            task_id: task_id.map(str::to_string),
            tool: tool.to_string(),
            kind: kind.to_string(),
            input_summary,
            result_summary,
            reason,
            affected_files: Vec::new(),
            created_at: timestamp(),
        };
        self.store.append_operation(&self.workspace_id, &operation)?;
        Ok(operation)
    }

    pub fn list_operations(
        &self,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<OperationRecord>> {
        self.store
            .list_operations(&self.workspace_id, offset, limit)
    }

    pub fn project_state(&self, max_files: usize) -> HarnessResult<ProjectState> {
        let current = self.capture_baseline();
        let task = self.current_task()?;
        let baseline_map = task
            .as_ref()
            .map(|t| {
                t.baseline
                    .entries
                    .iter()
                    .map(|e| (e.path.clone(), e))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let current_map: HashMap<_, _> = current
            .entries
            .iter()
            .map(|e| (e.path.clone(), e))
            .collect();
        let mut paths: Vec<String> = baseline_map
            .keys()
            .chain(current_map.keys())
            .cloned()
            .collect();
        paths.sort();
        paths.dedup();
        let total_files = paths.len();
        let files = paths
            .into_iter()
            .map(|path| {
                let before = baseline_map.get(&path).map(|e| e.sha256.clone());
                let entry = current_map.get(&path);
                let status = match (before, entry) {
                    (Some(before), Some(entry)) if before == entry.sha256 => "unchanged",
                    (Some(_), Some(_)) => "modified",
                    (Some(_), None) => "deleted",
                    (None, Some(_)) => "added",
                    (None, None) => "unknown",
                };
                ProjectFileState {
                    path,
                    status: status.to_string(),
                    sha256: entry.map(|e| e.sha256.clone()).unwrap_or_default(),
                    bytes: entry.map(|e| e.bytes).unwrap_or(0),
                }
            })
            .collect::<Vec<_>>();
        let truncated = files.len() > max_files.max(1);
        let files = files.into_iter().take(max_files.max(1)).collect::<Vec<_>>();
        let active_task_id = task.as_ref().map(|t| t.id.clone());
        let recent_events = task
            .as_ref()
            .and_then(|t| self.list_events(&t.id, 0, 100).ok())
            .map(|events| events.len())
            .unwrap_or(0);
        Ok(ProjectState {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            branch: current.branch,
            head: current.head,
            clean: files.iter().all(|f| f.status == "unchanged"),
            files,
            total_files,
            truncated,
            active_task_id,
            task,
            recent_events,
        })
    }

    pub fn status(&self) -> HarnessResult<HarnessStatus> {
        self.status_with_baseline(|| self.capture_baseline())
    }

    fn status_with_baseline(
        &self,
        capture: impl FnOnce() -> ProjectBaseline,
    ) -> HarnessResult<HarnessStatus> {
        let task = self.current_task()?;
        // Standalone errors/status need Git metadata, not a snapshot of every
        // file. A home-directory workspace may contain OS-protected media.
        let current = task.as_ref().map(|_| capture());
        let (branch, head) = match current.as_ref() {
            Some(current) => (current.branch.clone(), current.head.clone()),
            None => (
                git_value(&self.workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"]),
                git_value(&self.workspace_root, &["rev-parse", "HEAD"]),
            ),
        };
        let (task_id, task_state, task_updated_at, writable, baseline_matches, reason) =
            match task.as_ref() {
                Some(task) => {
                    let current = current.as_ref().expect("active task has a baseline");
                    let matches = task.baseline.branch == current.branch
                        && task.baseline.head == current.head
                        && task.expected_fingerprint == current.worktree_fingerprint;
                    let reason = if matches {
                        "任务可继续执行"
                    } else {
                        "工作区基线已变化，写入和执行已暂停"
                    };
                    (
                        Some(task.id.clone()),
                        Some(task.status),
                        Some(task.updated_at.clone()),
                        matches && task.status.is_writable(),
                        Some(matches),
                        reason.to_string(),
                    )
                }
                None => (
                    None,
                    None,
                    None,
                    true,
                    None,
                    "当前没有活动任务，工作区采用无任务模式；修改不会进入任务事件流".to_string(),
                ),
            };

        let mut capabilities = HashMap::new();
        capabilities.insert(
            "read".into(),
            CapabilityStatus {
                status: "available".into(),
                reason: "工作区读取不依赖活动任务".into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "write".into(),
            CapabilityStatus {
                status: if writable { "available" } else { "denied" }.into(),
                reason: if writable {
                    if task_id.is_some() {
                        "活动任务和工作区基线有效"
                    } else {
                        "无任务模式允许直接修改，建议需要长期追踪时调用 start_task"
                    }
                } else {
                    "需要活动任务且工作区基线必须匹配"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "exec".into(),
            CapabilityStatus {
                status: if writable { "available" } else { "denied" }.into(),
                reason: if writable {
                    if task_id.is_some() {
                        "活动任务和工作区基线有效"
                    } else {
                        "无任务模式允许直接执行，建议需要长期追踪时调用 start_task"
                    }
                } else {
                    "需要活动任务且工作区基线必须匹配"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "git".into(),
            CapabilityStatus {
                status: if branch.is_some() && head.is_some() {
                    "available"
                } else {
                    "degraded"
                }
                .into(),
                reason: if branch.is_some() && head.is_some() {
                    "已读取当前分支和 HEAD"
                } else {
                    "当前工作区不是可读取 Git 状态的仓库"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "network".into(),
            CapabilityStatus {
                status: "managed_by_policy".into(),
                reason: "网络权限由工具策略控制，不由 Harness 任务状态决定".into(),
                recoverable: true,
            },
        );

        let mut next_actions = Vec::new();
        if task_id.is_none() {
            next_actions.push("start_task".into());
        } else if baseline_matches == Some(false) {
            next_actions.push("project_state".into());
            next_actions.push("git_diff".into());
            next_actions.push("refresh_baseline".into());
        } else if !writable {
            next_actions.push("resume_task".into());
        }
        next_actions.push("read_file".into());
        next_actions.push("git_status".into());

        Ok(HarnessStatus {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            task_id,
            task_state,
            task_updated_at,
            writable,
            reason,
            recoverable: true,
            branch,
            head,
            baseline_matches,
            capabilities,
            next_actions,
        })
    }

    fn save_workspace_state(
        &self,
        active_task_id: Option<&str>,
        updated_at: &str,
    ) -> HarnessResult<()> {
        self.store.save_workspace_state(
            &self.workspace_id,
            &WorkspaceHarnessState {
                schema_version: SCHEMA_VERSION,
                active_task_id: active_task_id.map(str::to_string),
                recent_task_ids: self
                    .store
                    .list_tasks(&self.workspace_id)?
                    .into_iter()
                    .take(20)
                    .map(|t| t.id)
                    .collect(),
                updated_at: updated_at.to_string(),
            },
        )
    }

    fn capture_baseline(&self) -> ProjectBaseline {
        capture_baseline_with_ignored(&self.workspace_root, &self.ignored_paths)
    }
}

pub fn capture_baseline(root: &Path) -> ProjectBaseline {
    capture_baseline_with_ignored(root, &[])
}

fn capture_baseline_with_ignored(root: &Path, ignored_paths: &[PathBuf]) -> ProjectBaseline {
    let mut entries = Vec::new();
    // Prune skipped directories (OneDrive, node_modules, …) so WalkDir does not
    // descend into them — hashing alone is not enough for home-dir workspaces.
    for item in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.path() == root
                || (!should_skip(entry.path(), root)
                    && !ignored_paths.iter().any(|ignored| entry.path().starts_with(ignored)))
        })
        .filter_map(Result::ok)
    {
        let path = item.path();
        if path == root || !item.file_type().is_file() {
            continue;
        }
        let Some((sha256, is_binary, byte_len)) = hash_file_bounded(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        entries.push(BaselineEntry {
            path: rel,
            exists: true,
            is_binary,
            sha256,
            bytes: byte_len,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut fingerprint = Sha256::new();
    for entry in &entries {
        fingerprint.update(entry.path.as_bytes());
        fingerprint.update(entry.sha256.as_bytes());
        fingerprint.update(entry.bytes.to_le_bytes());
    }
    ProjectBaseline {
        branch: git_value(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        head: git_value(root, &["rev-parse", "HEAD"]),
        worktree_fingerprint: format!("{:x}", fingerprint.finalize()),
        entries,
        captured_at: timestamp(),
    }
}

/// Stream a file in fixed 64 KiB chunks: O(buffer) memory regardless of file
/// size, so a multi-GB file inside a large workspace cannot spike RSS during
/// baseline capture. Returns (sha256, is_binary, total_bytes).
fn hash_file_bounded(path: &Path) -> Option<(String, bool, u64)> {
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut is_binary = false;
    let mut total: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        if !is_binary && chunk.contains(&0) {
            is_binary = true;
        }
        hasher.update(chunk);
        total += read as u64;
    }
    Some((format!("{:x}", hasher.finalize()), is_binary, total))
}

fn should_skip(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .into_iter()
        .flat_map(|p| p.components())
        .filter_map(|component| component.as_os_str().to_str())
        .any(is_skipped_component)
}

fn is_skipped_component(name: &str) -> bool {
    if matches!(
        name,
        ".git"
            | ".mcp-probe-kit"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".svelte-kit"
            | ".cache"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".next"
            | ".turbo"
            | "coverage"
    ) {
        return true;
    }
    // OS / cloud roots: compare case-insensitively (Windows folder casing varies).
    matches!(
        name.to_ascii_lowercase().as_str(),
        "library"
            | "onedrive"
            | "onedrivetemp"
            | "appdata"
            | "application data"
            | "windows"
            | "program files"
            | "program files (x86)"
            | "programdata"
            | "$recycle.bin"
            | "system volume information"
    )
}

fn git_value(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn workspace_id(root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..32].to_string()
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn standalone_status_never_reads_workspace_file_contents() {
        let workspace = tempdir().unwrap();
        let harness_root = tempdir().unwrap();
        fs::create_dir(workspace.path().join("Music")).unwrap();
        fs::write(workspace.path().join("Music/private-library"), "unrelated data").unwrap();
        let harness = Harness::new(workspace.path().to_path_buf(), harness_root.path().to_path_buf()).unwrap();
        let status = harness.status_with_baseline(|| panic!("standalone status must not scan files")).unwrap();
        assert!(status.task_id.is_none());
        assert!(status.writable);
        assert_eq!(status.baseline_matches, None);
    }

    #[test]
    fn status_keeps_read_available_without_task() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");

        let status = harness.status().expect("status");
        assert!(status.writable);
        assert_eq!(status.capabilities["read"].status, "available");
        assert_eq!(status.capabilities["write"].status, "available");
        assert!(status.next_actions.contains(&"start_task".to_string()));
    }

    #[test]
    fn starting_task_does_not_create_workspace_copies() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");

        harness.start_task("测试任务").expect("start task");
        assert!(!harness
            .store_root()
            .join("workspaces")
            .join(harness.workspace_id())
            .join("snapshots")
            .exists());
    }

    #[test]
    fn capture_baseline_prunes_skipped_directories() {
        let root = tempdir().expect("root");
        fs::create_dir_all(root.path().join("OneDrive").join("deep")).expect("onedrive");
        fs::write(root.path().join("OneDrive").join("deep").join("cloud.bin"), vec![0u8; 1024])
            .expect("cloud file");
        fs::create_dir_all(root.path().join("node_modules").join("pkg")).expect("nm");
        fs::write(
            root.path().join("node_modules").join("pkg").join("index.js"),
            "module.exports=1\n",
        )
        .expect("nm file");
        fs::write(root.path().join("keep.txt"), "hello\n").expect("keep");

        let baseline = capture_baseline(root.path());
        let paths: Vec<&str> = baseline.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["keep.txt"]);
        assert!(is_skipped_component("onedrive"));
        assert!(is_skipped_component("OneDrive"));
        assert!(is_skipped_component("LIBRARY"));
    }
}
