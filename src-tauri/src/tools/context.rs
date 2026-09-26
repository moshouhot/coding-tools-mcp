use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::AuditStore;
use crate::harness::{Harness, HarnessResult};
use crate::tools::policy::PolicySettings;
use crate::tools::session::SessionStore;
use crate::tools::workspace::{relative_display, Workspace, WorkspaceError, WorkspaceResult};
use crate::workspace::AuthConfig;

#[derive(Clone, Default, Deserialize, Serialize)]
struct ActiveProjectSessions {
    paths: HashMap<String, PathBuf>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct ManagedProjectPaths {
    paths: HashMap<String, Vec<PathBuf>>,
}

impl ActiveProjectSessions {
    fn get(&self, session_key: &str) -> Option<PathBuf> {
        self.paths.get(session_key).cloned()
    }

    fn contains_key(&self, session_key: &str) -> bool {
        self.paths.contains_key(session_key)
    }

    fn insert(&mut self, session_key: String, path: PathBuf) {
        self.paths.insert(session_key, path);
    }
}

pub struct ToolContext {
    pub workspace: Workspace,
    pub auth: AuthConfig,
    pub policy: PolicySettings,
    pub tool_profile: String,
    pub permission_mode: String,
    pub harness: Harness,
    // 审计附着在现有 ToolContext，避免修改全部工具签名；生产监听器显式启用，测试与内部
    // 构造保持 None。正式 profile_id 绑定前暂用 Harness 的稳定工作区 ID 作为兼容标签。
    audit: Option<AuditStore>,
    audit_workspace_id: String,
    default_cwd: Mutex<PathBuf>,
    active_projects: Mutex<ActiveProjectSessions>,
    active_projects_file: PathBuf,
    active_projects_load_error: Mutex<Option<String>>,
    managed_project_paths: Mutex<HashMap<PathBuf, Vec<PathBuf>>>,
    managed_project_paths_file: PathBuf,
    pub sessions: Arc<SessionStore>,
}

pub type SharedToolContext = Arc<ToolContext>;

impl ToolContext {
    pub fn new(workspace_path: PathBuf) -> Result<Self, String> {
        let workspace = Workspace::new(workspace_path).map_err(|e| e.message())?;
        let auth = AuthConfig {
            auth_type: "noauth".into(),
            ..AuthConfig::default()
        };
        Ok(Self::from_workspace(
            workspace,
            auth,
            PolicySettings::default(),
            "full".into(),
            "trusted".into(),
        ))
    }

    pub fn from_workspace(
        workspace: Workspace,
        auth: AuthConfig,
        policy: PolicySettings,
        tool_profile: String,
        permission_mode: String,
    ) -> Self {
        let harness_root = Harness::default_root().expect("无法初始化 Harness 数据目录");
        Self::from_workspace_with_harness_root(
            workspace,
            auth,
            policy,
            crate::tools::registry::normalize_tool_profile(&tool_profile).into(),
            permission_mode,
            harness_root,
        )
    }

    pub fn from_workspace_with_harness_root(
        workspace: Workspace,
        auth: AuthConfig,
        policy: PolicySettings,
        tool_profile: String,
        permission_mode: String,
        harness_root: PathBuf,
    ) -> Self {
        let root = workspace.root().to_path_buf();
        let harness = Harness::new(root.clone(), harness_root).expect("无法初始化 Harness");
        let audit_workspace_id = harness.workspace_id().to_string();
        let active_projects_file = harness
            .store_root()
            .join("active-project-sessions")
            .join(format!("{}.json", harness.workspace_id()));
        let (active_projects, active_projects_load_error) = load_json_state(&active_projects_file)
            .map(|value| (value.unwrap_or_default(), None))
            .unwrap_or_else(|error| (ActiveProjectSessions::default(), Some(error)));
        let managed_project_paths_file = harness
            .store_root()
            .join("managed-project-paths")
            .join(format!("{}.json", harness.workspace_id()));
        let managed_project_paths = load_json_state::<ManagedProjectPaths>(&managed_project_paths_file)
            .ok()
            .flatten()
            .unwrap_or_default()
            .paths
            .into_iter()
            .map(|(root, paths)| (PathBuf::from(root), paths))
            .collect();
        Self {
            workspace,
            auth,
            policy,
            tool_profile: crate::tools::registry::normalize_tool_profile(&tool_profile).into(),
            permission_mode,
            harness,
            audit: None,
            audit_workspace_id,
            default_cwd: Mutex::new(root),
            active_projects: Mutex::new(active_projects),
            active_projects_file,
            active_projects_load_error: Mutex::new(active_projects_load_error),
            managed_project_paths: Mutex::new(managed_project_paths),
            managed_project_paths_file,
            sessions: Arc::new(SessionStore::new()),
        }
    }

    pub fn for_test(workspace_path: PathBuf, harness_root: PathBuf) -> Result<Self, String> {
        let workspace = Workspace::new(workspace_path).map_err(|e| e.message())?;
        Ok(Self::from_workspace_with_harness_root(
            workspace,
            AuthConfig {
                auth_type: "noauth".into(),
                ..AuthConfig::default()
            },
            PolicySettings::default(),
            "full".into(),
            "trusted".into(),
            harness_root,
        ))
    }

    pub fn workspace_path(&self) -> String {
        self.workspace.root_display()
    }

    // 此步骤位于 profile_id 已确定、Context 尚未进入 Arc 的构造末端；开库失败时降级为
    // 无审计，不能影响工具服务可用性。
    pub fn with_audit(mut self, workspace_id: impl Into<String>) -> Self {
        let workspace_id = workspace_id.into();
        if !workspace_id.trim().is_empty() {
            match AuditStore::open_default() {
                Ok(audit) => {
                    self.audit = Some(audit);
                }
                Err(error) => eprintln!("audit store disabled: {error}"),
            }
            self.audit_workspace_id = workspace_id;
        }
        self
    }

    pub fn audit_workspace_id(&self) -> &str {
        &self.audit_workspace_id
    }

    pub fn default_cwd_display(&self) -> String {
        let cwd = self.default_cwd.lock().expect("cwd lock");
        relative_display(self.workspace.root(), &cwd)
    }

    pub fn set_default_cwd(&self, path: PathBuf) {
        *self.default_cwd.lock().expect("cwd lock") = path;
    }

    pub fn default_cwd_path(&self) -> PathBuf {
        self.default_cwd.lock().expect("cwd lock").clone()
    }

    pub fn active_project_path(&self, session_key: Option<&str>) -> WorkspaceResult<PathBuf> {
        if let Some(session_key) = session_key.map(str::trim).filter(|value| !value.is_empty()) {
            if let Some(path) = self
                .active_projects
                .lock()
                .expect("active project lock")
                .get(session_key)
            {
                return self.validate_bound_project(session_key, &path);
            }
            if let Some(error) = self
                .active_projects_load_error
                .lock()
                .expect("active project store error lock")
                .clone()
            {
                return Err(WorkspaceError::ToolDetails {
                    code: "ACTIVE_PROJECT_STORE_UNAVAILABLE",
                    message: "Persisted Active Project bindings could not be loaded; refusing to fall back silently.".into(),
                    category: "state",
                    retryable: true,
                    details: json!({
                        "reason": error,
                        "suggestion": "Call set_active_project for this conversation to repair its binding."
                    }),
                });
            }
        }
        Ok(self.default_cwd_path())
    }

    pub fn active_project_display(&self, session_key: Option<&str>) -> WorkspaceResult<String> {
        let path = self.active_project_path(session_key)?;
        Ok(relative_display(self.workspace.root(), &path))
    }

    pub fn has_session_active_project(&self, session_key: &str) -> bool {
        self.active_projects
            .lock()
            .expect("active project lock")
            .contains_key(session_key)
    }

    pub fn set_session_active_project(
        &self,
        session_key: &str,
        path: PathBuf,
    ) -> WorkspaceResult<()> {
        let mut guard = self.active_projects.lock().expect("active project lock");
        let mut next = guard.clone();
        next.insert(session_key.to_string(), path);
        persist_json_state(&self.active_projects_file, &next)
            .map_err(active_project_store_error)?;
        *guard = next;
        Ok(())
    }

    pub fn harness_for_session(&self, session_key: Option<&str>) -> HarnessResult<Harness> {
        let Some(session_key) = session_key
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .filter(|value| self.has_session_active_project(value))
        else {
            return Ok(self.harness.clone());
        };
        let project_root = self.active_project_path(Some(session_key)).map_err(|error| {
            crate::harness::HarnessError::new(error_code(&error), error.message())
        })?;
        self.harness_for_project_root(&project_root)
    }

    pub fn harness_for_project_root(&self, project_root: &std::path::Path) -> HarnessResult<Harness> {
        if project_root == self.workspace.root() {
            return Ok(self.harness.clone());
        }
        let ignored_paths = self.managed_paths_for(project_root);
        Harness::new_with_ignored_paths(
            project_root.to_path_buf(),
            self.harness.store_root().to_path_buf(),
            ignored_paths,
        )
    }

    pub fn register_managed_project_path(
        &self,
        project_root: &std::path::Path,
        managed_path: &std::path::Path,
    ) -> WorkspaceResult<()> {
        if !managed_path.starts_with(project_root) {
            return Err(WorkspaceError::path_outside_workspace());
        }
        let mut registry = self
            .managed_project_paths
            .lock()
            .expect("managed project paths lock");
        let mut next = registry.clone();
        let paths = next.entry(project_root.to_path_buf()).or_default();
        if !paths.iter().any(|existing| existing == managed_path) {
            paths.push(managed_path.to_path_buf());
        }
        let persisted = ManagedProjectPaths {
            paths: next
                .iter()
                .map(|(root, paths)| (root.to_string_lossy().into_owned(), paths.clone()))
                .collect(),
        };
        persist_json_state(&self.managed_project_paths_file, &persisted).map_err(|reason| {
            WorkspaceError::ToolDetails {
                code: "MANAGED_PROJECT_PATH_STORE_UNAVAILABLE",
                message: "Unable to persist managed project path metadata.".into(),
                category: "state",
                retryable: true,
                details: json!({"reason": reason}),
            }
        })?;
        *registry = next;
        Ok(())
    }

    pub fn audit_store(&self) -> Option<AuditStore> {
        self.audit.clone()
    }

    fn validate_bound_project(&self, session_key: &str, path: &PathBuf) -> WorkspaceResult<PathBuf> {
        let canonical = path
            .canonicalize()
            .map_err(|_| active_project_unavailable(session_key, path))?;
        if !canonical.is_dir() || !canonical.starts_with(self.workspace.root()) {
            return Err(active_project_unavailable(session_key, path));
        }
        Ok(canonical)
    }

    fn managed_paths_for(&self, project_root: &std::path::Path) -> Vec<PathBuf> {
        self.managed_project_paths
            .lock()
            .expect("managed project paths lock")
            .get(project_root)
            .cloned()
            .unwrap_or_default()
    }
}

fn load_json_state<T: DeserializeOwned>(path: &std::path::Path) -> Result<Option<T>, String> {
    let backup = path.with_extension("json.bak");
    for candidate in [path, backup.as_path()] {
        if !candidate.exists() {
            continue;
        }
        match fs::read(candidate)
            .map_err(|error| error.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()))
        {
            Ok(value) => return Ok(Some(value)),
            Err(error) if candidate == path && backup.exists() => {
                eprintln!("state file invalid; trying backup: {error}");
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn persist_json_state<T: Serialize>(path: &std::path::Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "state file has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let temp = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");
    fs::write(&temp, bytes).map_err(|error| error.to_string())?;
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|error| error.to_string())?;
        }
        fs::rename(path, &backup).map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(&temp, path) {
        if !path.exists() && backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(error.to_string());
    }
    if backup.exists() {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn active_project_unavailable(session_key: &str, path: &std::path::Path) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "ACTIVE_PROJECT_UNAVAILABLE",
        message: "The project previously bound to this conversation is no longer available; refusing to fall back to another project.".into(),
        category: "not_found",
        retryable: true,
        details: json!({
            "session_key": session_key,
            "bound_project": path.display().to_string(),
            "suggestion": "Use set_active_project to explicitly rebind this conversation."
        }),
    }
}

fn active_project_store_error(reason: String) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "ACTIVE_PROJECT_STORE_UNAVAILABLE",
        message: "Unable to persist Active Project session binding.".into(),
        category: "state",
        retryable: true,
        details: json!({"reason": reason}),
    }
}

fn error_code(error: &WorkspaceError) -> &'static str {
    match error {
        WorkspaceError::Tool { code, .. } | WorkspaceError::ToolDetails { code, .. } => code,
    }
}
