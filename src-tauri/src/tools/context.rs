use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::audit::AuditStore;
use crate::harness::Harness;
use crate::tools::policy::PolicySettings;
use crate::tools::session::SessionStore;
use crate::tools::workspace::{relative_display, Workspace};
use crate::workspace::AuthConfig;

const MAX_ACTIVE_PROJECT_SESSIONS: usize = 256;

#[derive(Default)]
struct ActiveProjectSessions {
    paths: HashMap<String, PathBuf>,
    order: VecDeque<String>,
}

impl ActiveProjectSessions {
    fn get(&self, session_key: &str) -> Option<PathBuf> {
        self.paths.get(session_key).cloned()
    }

    fn contains_key(&self, session_key: &str) -> bool {
        self.paths.contains_key(session_key)
    }

    fn insert(&mut self, session_key: String, path: PathBuf) {
        if self.paths.contains_key(&session_key) {
            self.paths.insert(session_key, path);
            return;
        }
        while self.paths.len() >= MAX_ACTIVE_PROJECT_SESSIONS {
            let Some(oldest) = self.order.pop_front() else {
                self.paths.clear();
                break;
            };
            self.paths.remove(&oldest);
        }
        self.order.push_back(session_key.clone());
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
            active_projects: Mutex::new(ActiveProjectSessions::default()),
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

    pub fn active_project_path(&self, session_key: Option<&str>) -> PathBuf {
        if let Some(session_key) = session_key.map(str::trim).filter(|value| !value.is_empty()) {
            if let Some(path) = self
                .active_projects
                .lock()
                .expect("active project lock")
                .get(session_key)
            {
                return path;
            }
        }
        self.default_cwd_path()
    }

    pub fn active_project_display(&self, session_key: Option<&str>) -> String {
        let path = self.active_project_path(session_key);
        relative_display(self.workspace.root(), &path)
    }

    pub fn has_session_active_project(&self, session_key: &str) -> bool {
        self.active_projects
            .lock()
            .expect("active project lock")
            .contains_key(session_key)
    }

    pub fn set_session_active_project(&self, session_key: &str, path: PathBuf) {
        self.active_projects
            .lock()
            .expect("active project lock")
            .insert(session_key.to_string(), path);
    }

    pub fn audit_store(&self) -> Option<AuditStore> {
        self.audit.clone()
    }
}
