use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::settings::{DownloadConfig, FrpProfile, ProxyConfig};
use crate::workspace::WorkspaceProfile;

/// Unified on-disk payload stored in `data/profiles.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppData {
    #[serde(default)]
    pub frp_profiles: Vec<FrpProfile>,
    #[serde(default)]
    pub last_workspace_id: String,
    /// 开机自启绑定并启动的工作区 id。空表示未绑定。
    #[serde(default)]
    pub autostart_workspace_id: String,
    /// 上次**实际**在运行的 MCP 工作区 id。
    ///
    /// 记录的是真实观察到的运行状态（而非“用户点过启动”的意图），
    /// 因此强杀 / 断电后依旧准确。普通启动时据此恢复。
    #[serde(default)]
    pub running_mcp_workspace_ids: Vec<String>,
    #[serde(default)]
    pub download: DownloadConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub shared_secrets: HashMap<String, String>,
    #[serde(default)]
    pub workspace_secrets: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub app_secrets: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub profiles: Vec<WorkspaceProfile>,
}

/// Legacy `{ "profiles": [...] }` file at the app root.
#[derive(Debug, Deserialize)]
pub struct LegacyProfilesOnlyFile {
    pub profiles: Vec<WorkspaceProfile>,
}
