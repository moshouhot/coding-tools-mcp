use std::path::PathBuf;

use tauri::State;

use crate::app_state::{bootstrap_workspace, teardown_workspace, AppState};
use crate::audit::AuditStore;
use crate::error::{AppError, AppResult};
use crate::mcp::upstream::{DiscoveredUpstreamTool, UpstreamMcpManager};
use crate::platform::open_path_in_file_manager;
use crate::runtime::{await_listener_shutdown_for_delete, ServiceKind};
use crate::tunnel::drop_workspace as drop_tunnel_workspace;
use crate::workspace::resources::{
    assign_free_workspace_ports, validate_workspace_resources_update,
};
use crate::workspace::{validate_upstream_mcps, UpstreamMcpConfig, WorkspaceProfile};

#[tauri::command]
pub fn list_workspaces(state: State<'_, AppState>) -> AppResult<Vec<WorkspaceProfile>> {
    state.with_workspaces(|store| Ok(store.list().to_vec()))
}

#[tauri::command]
pub fn create_workspace(
    state: State<'_, AppState>,
    path: String,
    name: Option<String>,
) -> AppResult<WorkspaceProfile> {
    state.with_workspaces(|store| {
        let mut profile = WorkspaceProfile::new(path, name);
        // Create should not fail just because default ports are already claimed.
        // Pick free ports now; start/update still enforce conflict checks.
        assign_free_workspace_ports(store.list(), &mut profile)?;
        bootstrap_workspace(store, &profile.id)?;
        store.add(profile.clone())?;
        Ok(profile)
    })
}

#[tauri::command]
pub async fn update_workspace(state: State<'_, AppState>, profile: WorkspaceProfile) -> AppResult<()> {
    validate_upstream_mcps(&profile.runtime.upstream_mcps).map_err(AppError::Message)?;
    let current = state.with_workspaces(|store| {
        let current = store
            .get(&profile.id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {}", profile.id)))?;
        validate_workspace_resources_update(store.list(), &current, &profile)?;
        store.update(profile.clone())?;
        Ok(current)
    })?;

    // An enabled upstream is part of the MCP tool catalog. Reload a running
    // runtime immediately so the saved configuration and exposed tool list can
    // never silently diverge. Roll back persistence if the new configuration
    // cannot initialize.
    if current.runtime.upstream_mcps != profile.runtime.upstream_mcps {
        let is_running = state.with_runtime(|runtime| {
            Ok(runtime.is_running(&profile.id, crate::runtime::ServiceKind::Mcp))
        })?;
        if is_running {
            let restarted = crate::commands::runtime::restart_mcp_by_id(&state, &profile.id).await;
            if !matches!(restarted, Ok(ref status) if status.state == "running") {
                state.with_workspaces(|store| store.update(current.clone()))?;
                // The failed restart has already stopped the prior listener. Restore
                // the last known-good configuration before reporting failure.
                let _ = crate::commands::runtime::restart_mcp_by_id(&state, &profile.id).await;
                return Err(AppError::Message(
                    "本地 MCP 配置无法启动，已恢复为上一次有效配置".to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Probe an unsaved stdio configuration so the workspace owner can choose
/// individual tools before exposing the MCP. The short-lived child is always
/// shut down by `UpstreamMcpManager::inspect`.
#[tauri::command]
pub async fn discover_upstream_tools(
    config: UpstreamMcpConfig,
) -> AppResult<Vec<DiscoveredUpstreamTool>> {
    UpstreamMcpManager::inspect(&config)
        .await
        .map_err(AppError::Message)
}

#[tauri::command]
pub fn open_workspace_directory(path: String) -> AppResult<()> {
    let path = PathBuf::from(path.trim());
    open_path_in_file_manager(&path)
}

#[tauri::command]
pub async fn delete_workspace(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let profile = state.with_workspaces(|store| {
        store
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })?;
    drop_tunnel_workspace(&id).await?;
    // listener 退出会等待在途请求及其日志写入；完成后才能删除 profile 日志目录。
    for (kind, port) in [
        (ServiceKind::Mcp, profile.runtime.local_port),
        (ServiceKind::Actions, profile.actions.local_port),
    ] {
        let handle = state.with_runtime(|runtime| Ok(runtime.begin_stop(&id, kind)))?;
        await_listener_shutdown_for_delete(handle, port).await;
        state.with_runtime(|runtime| {
            runtime.finish_stop(&id, kind);
            Ok(())
        })?;
    }
    state.with_workspaces(|store| {
        if store.remove(&id)?.is_some() {
            teardown_workspace(store, &id)?;
        }
        Ok(())
    })?;

    // 日志是 profile 删除后的旁路清理；失败只告警，不能回滚已完成的工作区删除。
    if let Err(error) = crate::access_log::remove_workspace_logs(&id) {
        eprintln!("workspace access log cleanup failed: {error}");
    }
    // 工作区已删除，立即重记运行集合，把这个 id 从待恢复集合里清掉，
    // 避免下次启动把它当成「目标已失效」再报一次。
    crate::tray::record_after_transition();
    match AuditStore::open_default() {
        Ok(audit) => {
            if let Err(error) = audit.remove_workspace_records(&id) {
                eprintln!("workspace audit cleanup failed: {error}");
            }
        }
        Err(error) => eprintln!("workspace audit cleanup skipped: {error}"),
    }
    Ok(())
}
