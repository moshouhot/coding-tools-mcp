use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::audit::AuditStore;
use crate::harness::{Harness, HarnessResult};
use crate::tools::policy::PolicySettings;
use crate::tools::session::SessionStore;
use crate::tools::workspace::{relative_display, Workspace, WorkspaceError, WorkspaceResult};
use crate::workspace::AuthConfig;

#[derive(Clone, Deserialize, Serialize)]
struct ActiveProjectBinding {
    version: u8,
    session_key: String,
    binding_path: PathBuf,
    canonical_target: PathBuf,
}

fn active_project_required(session_key: &str) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "ACTIVE_PROJECT_REQUIRED",
        message: "This conversation has no Active Project binding; refusing to fall back to default_cwd.".into(),
        category: "state",
        retryable: true,
        details: json!({
            "session_key": session_key,
            "suggestion": "Bind the conversation with set_active_project before project-scoped work."
        }),
    }
}

fn active_project_target_changed(
    session_key: &str,
    binding_path: &std::path::Path,
    expected: &std::path::Path,
    actual: &std::path::Path,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "ACTIVE_PROJECT_TARGET_CHANGED",
        message: "The bound project path now resolves to a different target; refusing to follow it silently.".into(),
        category: "state",
        retryable: true,
        details: json!({
            "session_key": session_key,
            "binding_path": binding_path.display().to_string(),
            "expected_target": expected.display().to_string(),
            "actual_target": actual.display().to_string(),
            "suggestion": "Verify the path and explicitly rebind the conversation if the change is intentional."
        }),
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
    active_projects_dir: PathBuf,
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
        let active_projects_dir = harness
            .store_root()
            .join("active-project-sessions")
            .join(harness.workspace_id());
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
            active_projects_dir,
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
            return self
                .session_active_project_path(session_key)?
                .ok_or_else(|| active_project_required(session_key));
        }
        Ok(self.default_cwd_path())
    }

    pub fn active_project_display(&self, session_key: Option<&str>) -> WorkspaceResult<String> {
        let path = self.active_project_path(session_key)?;
        Ok(relative_display(self.workspace.root(), &path))
    }

    pub fn has_session_active_project(&self, session_key: &str) -> bool {
        let path = self.active_project_binding_file(session_key);
        path.exists() || path.with_extension("json.bak").exists()
    }

    pub fn set_session_active_project(
        &self,
        session_key: &str,
        path: PathBuf,
    ) -> WorkspaceResult<()> {
        let canonical = path
            .canonicalize()
            .map_err(|_| active_project_unavailable(session_key, &path))?;
        self.set_session_active_project_binding(session_key, path, canonical)
    }

    pub fn set_session_active_project_binding(
        &self,
        session_key: &str,
        binding_path: PathBuf,
        canonical_target: PathBuf,
    ) -> WorkspaceResult<()> {
        let binding = ActiveProjectBinding {
            version: 1,
            session_key: session_key.to_string(),
            binding_path,
            canonical_target,
        };
        persist_json_state(&self.active_project_binding_file(session_key), &binding)
            .map_err(active_project_store_error)
    }

    pub fn session_active_project_path(
        &self,
        session_key: &str,
    ) -> WorkspaceResult<Option<PathBuf>> {
        let Some(binding) = self.load_active_project_binding(session_key)? else {
            return Ok(None);
        };
        Ok(Some(self.validate_bound_project(session_key, &binding)?))
    }

    pub fn stored_session_project_target(
        &self,
        session_key: &str,
    ) -> WorkspaceResult<Option<PathBuf>> {
        Ok(self
            .load_active_project_binding(session_key)?
            .map(|binding| binding.canonical_target))
    }

    pub fn harness_for_session(&self, session_key: Option<&str>) -> HarnessResult<Harness> {
        let Some(session_key) = session_key.map(str::trim).filter(|value| !value.is_empty()) else {
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
        Harness::new(
            project_root.to_path_buf(),
            self.harness.store_root().to_path_buf(),
        )
    }

    pub fn audit_store(&self) -> Option<AuditStore> {
        self.audit.clone()
    }

    fn active_project_binding_file(&self, session_key: &str) -> PathBuf {
        let digest = Sha256::digest(session_key.as_bytes());
        self.active_projects_dir.join(format!("{digest:x}.json"))
    }

    fn load_active_project_binding(
        &self,
        session_key: &str,
    ) -> WorkspaceResult<Option<ActiveProjectBinding>> {
        let path = self.active_project_binding_file(session_key);
        let binding = load_json_state::<ActiveProjectBinding>(&path).map_err(|reason| {
            WorkspaceError::ToolDetails {
                code: "ACTIVE_PROJECT_STORE_UNAVAILABLE",
                message: "Unable to load this conversation's Active Project binding.".into(),
                category: "state",
                retryable: true,
                details: json!({
                    "reason": reason,
                    "suggestion": "After confirming the intended project, explicitly repair this conversation with set_active_project and allow_rebind=true."
                }),
            }
        })?;
        let Some(binding) = binding else {
            return Ok(None);
        };
        if binding.session_key != session_key {
            return Err(WorkspaceError::ToolDetails {
                code: "ACTIVE_PROJECT_STORE_UNAVAILABLE",
                message: "Active Project binding identity mismatch.".into(),
                category: "state",
                retryable: false,
                details: json!({"suggestion": "Explicitly rebind this conversation."}),
            });
        }
        Ok(Some(binding))
    }

    fn validate_bound_project(
        &self,
        session_key: &str,
        binding: &ActiveProjectBinding,
    ) -> WorkspaceResult<PathBuf> {
        let canonical = binding
            .binding_path
            .canonicalize()
            .map_err(|_| active_project_unavailable(session_key, &binding.binding_path))?;
        if !canonical.is_dir() || !canonical.starts_with(self.workspace.root()) {
            return Err(active_project_unavailable(session_key, &binding.binding_path));
        }
        if canonical != binding.canonical_target {
            return Err(active_project_target_changed(
                session_key,
                &binding.binding_path,
                &binding.canonical_target,
                &canonical,
            ));
        }
        Ok(canonical)
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
    let temp = path.with_extension(format!("json.{}.tmp", Uuid::new_v4().simple()));
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
