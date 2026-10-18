use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tauri::async_runtime::JoinHandle;

use crate::actions;
use crate::error::AppResult;
use crate::mcp;
use crate::mcp::upstream::UpstreamMcpManager;
use crate::platform::platform;
use crate::runtime::port::{
    is_own_process, port_busy_message, try_reclaim_previous_macos_app_port,
    wait_for_port_free_blocking,
};
use crate::secret::SecretStore;
use crate::tools::policy::PolicySettings;
use crate::tunnel::{append_profile_log, cleanup_orphan_for_runtime, TunnelServiceKind};
use crate::workspace::{RuntimeStatusDto, WorkspaceProfile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    Mcp,
    Actions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimePhase {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}

/// 单次「意图周期」内最多保活尝试次数，防止配置性问题导致无限重试。
const MAX_KEEPALIVE_ATTEMPTS: u8 = 3;
/// 两次保活尝试之间的最小间隔，避免重启风暴。
const KEEPALIVE_COOLDOWN: Duration = Duration::from_secs(60);

struct RuntimeEntry {
    phase: RuntimePhase,
    shutdown: Option<mcp::ShutdownSender>,
    handle: Option<JoinHandle<()>>,
    error_message: Option<String>,
    started_at: Option<std::time::Instant>,
    missing_port_checks: u8,
    /// 本次启动是否**真的**监听成功过。
    ///
    /// 用于区分「启动就失败」（不该自动重启，否则会无限重试）
    /// 与「曾经正常运行后意外死亡」（可以保活恢复）。
    had_listening: bool,
    /// 保活恢复尝试次数，成功监听或用户重新启动时清零。
    keepalive_attempts: u8,
    /// 上次保活尝试时间，用于冷却期，避免重启风暴。
    last_keepalive_at: Option<std::time::Instant>,
}

#[derive(Default)]
pub struct RuntimeSupervisor {
    entries: HashMap<(String, ServiceKind), RuntimeEntry>,
}

impl RuntimeSupervisor {
    pub fn mcp_status(&self, profile: &WorkspaceProfile) -> RuntimeStatusDto {
        self.status(profile, ServiceKind::Mcp)
    }

    pub fn actions_status(&self, profile: &WorkspaceProfile) -> RuntimeStatusDto {
        self.status(profile, ServiceKind::Actions)
    }

    /// Starts the listener after the async command layer has completed stdio
    /// initialization. Keeping that await out of this synchronous supervisor
    /// avoids nesting a runtime `block_on` while a Tauri command is executing.
    pub fn start_prepared_mcp(
        &mut self,
        profile: &WorkspaceProfile,
        upstream: Arc<UpstreamMcpManager>,
    ) -> AppResult<RuntimeStatusDto> {
        self.start(profile, ServiceKind::Mcp, Some(upstream))
    }

    pub fn start_actions(&mut self, profile: &WorkspaceProfile) -> AppResult<RuntimeStatusDto> {
        self.start(profile, ServiceKind::Actions, None)
    }

    /// True when the service for this workspace is currently running.
    pub fn is_running(&self, workspace_id: &str, kind: ServiceKind) -> bool {
        matches!(
            self.entries
                .get(&(workspace_id.to_string(), kind))
                .map(|entry| &entry.phase),
            Some(RuntimePhase::Running)
        )
    }

    pub fn refresh_mcp(&mut self, profile: &WorkspaceProfile) {
        self.refresh(profile, ServiceKind::Mcp);
    }

    pub fn refresh_actions(&mut self, profile: &WorkspaceProfile) {
        self.refresh(profile, ServiceKind::Actions);
    }

    /// 上游原有的同步工作区停止入口。
    ///
    /// 删除工作区现改用异步 begin_stop、等待在途请求与日志落盘、再 finish_stop；保留此
    /// 方法供维护者决定后续兼容策略，因此当前 Audit 流程不再调用它。
    pub fn drop_workspace(&mut self, profile: &WorkspaceProfile) {
        self.sync_stop_and_wait(profile, ServiceKind::Mcp);
        self.sync_stop_and_wait(profile, ServiceKind::Actions);
    }

    pub fn set_mcp_error(&mut self, profile: &WorkspaceProfile, message: String) {
        self.entries.insert(
            (profile.id.clone(), ServiceKind::Mcp),
            RuntimeEntry {
                phase: RuntimePhase::Error,
                shutdown: None,
                handle: None,
                error_message: Some(message),
                started_at: None,
                missing_port_checks: 0,
                had_listening: false,
                keepalive_attempts: 0,
                last_keepalive_at: None,
            },
        );
    }

    /// 取出应当尝试保活恢复的服务（MCP / Actions）。
    ///
    /// 只挑「曾经真的监听成功、随后进入 Error」的条目：
    /// - 启动就失败（`had_listening == false`）不重启，避免对配置错误无限重试；
    /// - 用户显式停止会落到 `Stopped`，因此不会被这里选中；
    /// - 冷却期内不重复尝试，且有次数上限，避免重启风暴。
    ///
    /// 本方法有副作用：选中即计一次尝试并记录时间，保证并发调用不会重复启动同一服务。
    pub fn take_keepalive_targets(
        &mut self,
        profiles: &[WorkspaceProfile],
        now: std::time::Instant,
    ) -> Vec<(WorkspaceProfile, ServiceKind)> {
        let mut targets = Vec::new();
        for profile in profiles {
            for kind in [ServiceKind::Mcp, ServiceKind::Actions] {
                let key = (profile.id.clone(), kind);
                let Some(entry) = self.entries.get_mut(&key) else {
                    continue;
                };
                if entry.phase != RuntimePhase::Error || !entry.had_listening {
                    continue;
                }
                if entry.keepalive_attempts >= MAX_KEEPALIVE_ATTEMPTS {
                    continue;
                }
                if let Some(last) = entry.last_keepalive_at {
                    if now.duration_since(last) < KEEPALIVE_COOLDOWN {
                        continue;
                    }
                }
                entry.keepalive_attempts += 1;
                entry.last_keepalive_at = Some(now);
                targets.push((profile.clone(), kind));
            }
        }
        targets
    }

    pub fn active_tunnel_service_keys(&self) -> HashSet<(String, TunnelServiceKind)> {
        self.entries
            .iter()
            .filter_map(|((workspace_id, kind), entry)| match entry.phase {
                RuntimePhase::Running | RuntimePhase::Starting => Some((
                    workspace_id.clone(),
                    match kind {
                        ServiceKind::Mcp => TunnelServiceKind::Mcp,
                        ServiceKind::Actions => TunnelServiceKind::Actions,
                    },
                )),
                _ => None,
            })
            .collect()
    }

    pub fn begin_stop(&mut self, workspace_id: &str, kind: ServiceKind) -> Option<JoinHandle<()>> {
        let key = (workspace_id.to_string(), kind);
        let entry = self.entries.get_mut(&key)?;

        entry.phase = RuntimePhase::Stopping;
        let shutdown = entry.shutdown.take();
        let handle = entry.handle.take();
        if let Some(shutdown) = shutdown {
            let _ = shutdown.send(());
        }
        handle
    }

    pub fn finish_stop(&mut self, workspace_id: &str, kind: ServiceKind) {
        self.entries.remove(&(workspace_id.to_string(), kind));
    }

    fn status(&self, profile: &WorkspaceProfile, kind: ServiceKind) -> RuntimeStatusDto {
        let key = (profile.id.clone(), kind);
        let phase = self
            .entries
            .get(&key)
            .map(|entry| entry.phase.clone())
            .unwrap_or(RuntimePhase::Stopped);

        let (local_endpoint, public_endpoint) = endpoints(profile, kind);
        let port = port_for(profile, kind);
        let service_label = service_label(kind);

        match phase {
            RuntimePhase::Running => RuntimeStatusDto {
                state: "running".into(),
                pid: None,
                local_message: format!("{service_label}正在监听 127.0.0.1:{port}"),
                public_message: public_message_for(profile, kind),
                local_endpoint,
                public_endpoint,
            },
            RuntimePhase::Starting => RuntimeStatusDto {
                state: "starting".into(),
                pid: None,
                local_message: format!("正在启动{service_label}端口 {port}"),
                public_message: "等待服务就绪".into(),
                local_endpoint,
                public_endpoint,
            },
            RuntimePhase::Stopping => RuntimeStatusDto {
                state: "stopping".into(),
                pid: None,
                local_message: "正在停止".into(),
                public_message: "正在停止".into(),
                local_endpoint,
                public_endpoint,
            },
            RuntimePhase::Error => {
                let message = self
                    .entries
                    .get(&key)
                    .and_then(|entry| entry.error_message.clone())
                    .unwrap_or_else(|| "运行失败".into());
                RuntimeStatusDto {
                    state: "error".into(),
                    pid: None,
                    local_message: message.clone(),
                    public_message: message,
                    local_endpoint,
                    public_endpoint,
                }
            }
            RuntimePhase::Stopped => RuntimeStatusDto {
                state: "stopped".into(),
                pid: None,
                local_message: "未启动".into(),
                public_message: "未知".into(),
                local_endpoint,
                public_endpoint,
            },
        }
    }

    fn start(
        &mut self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
        prepared_upstream: Option<Arc<UpstreamMcpManager>>,
    ) -> AppResult<RuntimeStatusDto> {
        let key = (profile.id.clone(), kind);
        if matches!(
            self.entries.get(&key).map(|e| &e.phase),
            Some(RuntimePhase::Running) | Some(RuntimePhase::Starting)
        ) {
            return Ok(self.status(profile, kind));
        }
        if matches!(
            self.entries.get(&key).map(|e| &e.phase),
            Some(RuntimePhase::Stopping)
        ) {
            return Err(crate::error::AppError::Message(format!(
                "{}正在停止，请稍后再试",
                service_label(kind).trim()
            )));
        }

        self.entries.insert(
            key.clone(),
            RuntimeEntry {
                phase: RuntimePhase::Starting,
                shutdown: None,
                handle: None,
                error_message: None,
                started_at: Some(std::time::Instant::now()),
                missing_port_checks: 0,
                had_listening: false,
                keepalive_attempts: 0,
                last_keepalive_at: None,
            },
        );

        let port = port_for(profile, kind);
        if let Some(pid) = platform().find_pid_listening_on_port(port)? {
            if is_own_process(pid) {
                wait_for_port_free_blocking(port, Duration::from_secs(3));
            }
            if try_reclaim_previous_macos_app_port(port) {
                // A previous source-built or installed instance of this macOS
                // app released the port; continue with the current listener.
            }
            if let Some(pid) = platform().find_pid_listening_on_port(port)? {
                self.entries.remove(&key);
                let message = port_busy_message(port, service_label(kind).trim(), pid);
                append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    &format!("[start] {message}"),
                );
                return Err(crate::error::AppError::Message(message));
            }
        }

        let spawn_result = match kind {
            ServiceKind::Mcp => {
                let use_shared = profile.auth.use_shared_secrets;
                let mut auth = profile.auth.clone();
                if use_shared {
                    if let Some(client_id) = SecretStore::get_shared("oauth_client_id")? {
                        auth.oauth_client_id = client_id;
                    }
                }
                // MCP OAuth matches legacy Python: client_secret is optional.
                // ChatGPT connectors use PKCE only and do not send client_secret.
                let oauth_client_secret = None;
                let oauth_password = if profile.auth.oauth_enabled() {
                    resolve_secret(&profile.id, "oauth_password", use_shared)?
                } else {
                    None
                };
                let oauth_token_secret = if profile.auth.oauth_enabled() {
                    resolve_secret(&profile.id, "oauth_token_secret", use_shared)?
                } else {
                    None
                };
                let upstream = prepared_upstream
                    .unwrap_or_else(|| Arc::new(UpstreamMcpManager::empty()));
                mcp::spawn_listener(
                    port,
                    PathBuf::from(&profile.path),
                    profile.id.clone(),
                    auth,
                    profile.effective_public_url(),
                    oauth_client_secret,
                    oauth_password,
                    oauth_token_secret,
                    profile.runtime.clone(),
                    upstream,
                )
            }
            ServiceKind::Actions => {
                let auth_type = profile.actions.auth_type.clone();
                let use_shared = profile.actions.use_shared_secrets;
                let api_key = if auth_type == "api_key" {
                    resolve_secret(&profile.id, "actions_api_key", use_shared)?
                } else {
                    None
                };
                let oauth_client_secret = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_client_secret", true)?
                    } else {
                        Some(actions_oauth_secret(
                            &profile.id,
                            "actions_oauth_client_secret",
                        )?)
                    }
                } else {
                    None
                };
                let oauth_password = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_password", true)?
                    } else {
                        Some(actions_oauth_secret(&profile.id, "actions_oauth_password")?)
                    }
                } else {
                    None
                };
                let oauth_token_secret = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_token_secret", true)?
                    } else {
                        Some(actions_oauth_secret(
                            &profile.id,
                            "actions_oauth_token_secret",
                        )?)
                    }
                } else {
                    None
                };
                let public_base_url = profile.actions_public_base_url();
                let policy = PolicySettings::from_actions_config(&profile.actions);
                actions::spawn_listener(
                    &profile.id,
                    port,
                    PathBuf::from(&profile.path),
                    public_base_url,
                    auth_type,
                    api_key,
                    profile.actions.oauth_client_id.clone(),
                    oauth_client_secret,
                    oauth_password,
                    oauth_token_secret,
                    policy,
                )
            }
        };

        match spawn_result {
            Ok((shutdown, handle)) => {
                let started_at = self
                    .entries
                    .get(&key)
                    .and_then(|entry| entry.started_at)
                    .or_else(|| Some(std::time::Instant::now()));
                self.entries.insert(
                    key,
                    RuntimeEntry {
                        phase: RuntimePhase::Running,
                        shutdown: Some(shutdown),
                        handle: Some(handle),
                        error_message: None,
                        started_at,
                        missing_port_checks: 0,
                        had_listening: false,
                        keepalive_attempts: 0,
                        last_keepalive_at: None,
                    },
                );
            }
            Err(err) => {
                // spawn_listener can fail synchronously before the server task is
                // ever created (e.g. missing API key / OAuth secret). In that case
                // serve() never runs, so nothing writes to the stderr log and the
                // failure was previously invisible in the log viewer. Record it here.
                append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    &format!("[start] {}启动失败：{err}", service_label(kind).trim()),
                );
                self.entries.insert(
                    key,
                    RuntimeEntry {
                        phase: RuntimePhase::Error,
                        shutdown: None,
                        handle: None,
                        error_message: Some(err.to_string()),
                        started_at: None,
                        missing_port_checks: 0,
                        had_listening: false,
                        keepalive_attempts: 0,
                        last_keepalive_at: None,
                    },
                );
            }
        }

        Ok(self.status(profile, kind))
    }

    fn sync_stop_and_wait(&mut self, profile: &WorkspaceProfile, kind: ServiceKind) {
        let port = port_for(profile, kind);
        let handle = self.begin_stop(&profile.id, kind);
        if handle.is_some() {
            crate::runtime::port::await_listener_shutdown_blocking(handle, port);
        } else if platform()
            .find_pid_listening_on_port(port)
            .ok()
            .flatten()
            .is_some()
        {
            wait_for_port_free_blocking(port, Duration::from_secs(3));
        }
        self.finish_stop(&profile.id, kind);
    }

    fn refresh(&mut self, profile: &WorkspaceProfile, kind: ServiceKind) {
        let key = (profile.id.clone(), kind);
        let port = port_for(profile, kind);
        let mut should_cleanup_tunnel = false;
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.phase == RuntimePhase::Running {
                let listening = match platform().find_pid_listening_on_port(port) {
                    Ok(pid) => pid.is_some(),
                    Err(error) => {
                        append_profile_log(
                            &profile.id,
                            stderr_log_name(kind),
                            &format!("[refresh] 检查端口 {port} 失败，保留当前线路：{error}"),
                        );
                        return;
                    }
                };
                if should_mark_runtime_error(entry, listening) {
                    if let Some(handle) = entry.handle.take() {
                        handle.abort();
                        tauri::async_runtime::spawn(async move {
                            let _ = handle.await;
                        });
                    }
                    entry.shutdown.take();
                    let occupied_by_self = platform()
                        .find_pid_listening_on_port(port)
                        .ok()
                        .flatten()
                        .map(is_own_process)
                        .unwrap_or(false);
                    let message = if occupied_by_self {
                        format!(
                            "{}端口 {} 未能成功启动，可能仍被本应用上一次服务占用，请先停止后再试",
                            service_label(kind).trim(),
                            port
                        )
                    } else {
                        format!(
                            "{}端口 {} 未能成功启动，可能已被其他程序占用",
                            service_label(kind).trim(),
                            port
                        )
                    };
                    entry.phase = RuntimePhase::Error;
                    entry.error_message = Some(message);
                    entry.started_at = None;
                    should_cleanup_tunnel = true;
                }
            }
        }

        // 状态查询本身不能改变其他工作区的隧道集合。只有本次刷新确认了
        // 一个原本 Running 的 runtime 已经进入 Error，才清理它对应的孤儿线路。
        // 之前无条件调用 cleanup_orphan 会把启动时的瞬时端口检测失败误认为
        // 孤儿 runtime，删除 route 后重启唯一的 frpc，导致其他工作区公网线路消失。
        if !should_cleanup_tunnel {
            return;
        }

        let tunnel_kind = match kind {
            ServiceKind::Mcp => TunnelServiceKind::Mcp,
            ServiceKind::Actions => TunnelServiceKind::Actions,
        };

        let profile = profile.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = cleanup_orphan_for_runtime(&profile, tunnel_kind, false).await {
                append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    &format!("[refresh] 清理失效隧道失败：{error}"),
                );
            }
        });
    }
}

fn should_mark_runtime_error(entry: &mut RuntimeEntry, listening: bool) -> bool {
    if entry.phase != RuntimePhase::Running {
        return false;
    }
    if listening {
        entry.missing_port_checks = 0;
        // 真的监听成功过：此后若意外死亡才值得保活恢复。
        entry.had_listening = true;
        entry.keepalive_attempts = 0;
        return false;
    }

    entry.missing_port_checks = entry.missing_port_checks.saturating_add(1);
    entry.missing_port_checks >= 3
        && entry
            .started_at
            .map(|started| started.elapsed() > Duration::from_millis(200))
            .unwrap_or(true)
}

fn port_for(profile: &WorkspaceProfile, kind: ServiceKind) -> u16 {
    match kind {
        ServiceKind::Mcp => profile.runtime.local_port,
        ServiceKind::Actions => profile.actions.local_port,
    }
}

fn endpoints(profile: &WorkspaceProfile, kind: ServiceKind) -> (String, String) {
    match kind {
        ServiceKind::Mcp => (profile.local_endpoint(), profile.public_endpoint()),
        ServiceKind::Actions => (
            profile.actions_local_base_url(),
            profile.actions_openapi_url(),
        ),
    }
}

fn public_message_for(profile: &WorkspaceProfile, kind: ServiceKind) -> String {
    match kind {
        ServiceKind::Mcp => profile.effective_public_url(),
        ServiceKind::Actions => profile.actions_effective_public_url(),
    }
}

fn service_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "本地 MCP ",
        ServiceKind::Actions => "本地 Actions ",
    }
}

fn stderr_log_name(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "stderr.log",
        ServiceKind::Actions => "actions-stderr.log",
    }
}

/// Resolve a secret from the shared pool or per-workspace keyring.
fn resolve_secret(profile_id: &str, key: &str, use_shared: bool) -> AppResult<Option<String>> {
    if use_shared {
        SecretStore::get_shared(key)
    } else {
        SecretStore::get(profile_id, key)
    }
}

fn actions_oauth_secret(profile_id: &str, key: &str) -> AppResult<String> {
    match SecretStore::get(profile_id, key)? {
        Some(value) if !value.is_empty() => Ok(value),
        _ => SecretStore::regenerate(profile_id, key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(phase: RuntimePhase, started_at: Option<std::time::Instant>) -> RuntimeEntry {
        RuntimeEntry {
            phase,
            shutdown: None,
            handle: None,
            error_message: None,
            started_at,
            missing_port_checks: 0,
            had_listening: false,
            keepalive_attempts: 0,
            last_keepalive_at: None,
        }
    }

    #[test]
    fn keepalive_skips_services_that_never_listened() {
        // 启动就失败（配置错、端口冲突）不能自动重启，否则会无限重试。
        let profile = WorkspaceProfile::new("C:/workspace/ka1".into(), Some("ka1".into()));
        let mut supervisor = RuntimeSupervisor::default();
        supervisor.entries.insert(
            (profile.id.clone(), ServiceKind::Mcp),
            entry(RuntimePhase::Error, None),
        );
        let targets = supervisor.take_keepalive_targets(&[profile.clone()], std::time::Instant::now());
        assert!(targets.is_empty(), "从未监听成功过的服务不应被保活恢复");
    }

    #[test]
    fn keepalive_targets_a_service_that_listened_then_died() {
        // 正常运行后意外死亡：这才是保活的目标。
        let profile = WorkspaceProfile::new("C:/workspace/ka2".into(), Some("ka2".into()));
        let mut supervisor = RuntimeSupervisor::default();
        let mut runtime_entry = entry(RuntimePhase::Error, None);
        runtime_entry.had_listening = true;
        supervisor
            .entries
            .insert((profile.id.clone(), ServiceKind::Mcp), runtime_entry);

        let now = std::time::Instant::now();
        let targets = supervisor.take_keepalive_targets(std::slice::from_ref(&profile), now);
        assert_eq!(targets.len(), 1, "应选中一个待恢复服务");
        assert_eq!(targets[0].1, ServiceKind::Mcp);
    }

    #[test]
    fn keepalive_respects_cooldown_between_attempts() {
        // 冷却期内不得重复尝试，避免重启风暴。
        let profile = WorkspaceProfile::new("C:/workspace/ka3".into(), Some("ka3".into()));
        let mut supervisor = RuntimeSupervisor::default();
        let mut runtime_entry = entry(RuntimePhase::Error, None);
        runtime_entry.had_listening = true;
        supervisor
            .entries
            .insert((profile.id.clone(), ServiceKind::Mcp), runtime_entry);

        let now = std::time::Instant::now();
        assert_eq!(
            supervisor.take_keepalive_targets(std::slice::from_ref(&profile), now).len(),
            1
        );
        // 同一时刻再取一次：冷却中，应为空。
        assert!(
            supervisor
                .take_keepalive_targets(std::slice::from_ref(&profile), now)
                .is_empty(),
            "冷却期内不得再次恢复"
        );
    }

    #[test]
    fn keepalive_stops_after_attempt_cap() {
        // 达到上限后不再尝试，避免无限重启。
        let profile = WorkspaceProfile::new("C:/workspace/ka4".into(), Some("ka4".into()));
        let mut supervisor = RuntimeSupervisor::default();
        let mut runtime_entry = entry(RuntimePhase::Error, None);
        runtime_entry.had_listening = true;
        runtime_entry.keepalive_attempts = MAX_KEEPALIVE_ATTEMPTS;
        supervisor
            .entries
            .insert((profile.id.clone(), ServiceKind::Mcp), runtime_entry);

        assert!(
            supervisor
                .take_keepalive_targets(std::slice::from_ref(&profile), std::time::Instant::now())
                .is_empty(),
            "达到尝试上限后必须停止恢复"
        );
    }

    #[test]
    fn keepalive_ignores_user_stopped_services() {
        // 用户显式停止 → Stopped，绝不能被保活复活。
        let profile = WorkspaceProfile::new("C:/workspace/ka5".into(), Some("ka5".into()));
        let mut supervisor = RuntimeSupervisor::default();
        let mut runtime_entry = entry(RuntimePhase::Stopped, None);
        runtime_entry.had_listening = true;
        supervisor
            .entries
            .insert((profile.id.clone(), ServiceKind::Mcp), runtime_entry);

        assert!(
            supervisor
                .take_keepalive_targets(std::slice::from_ref(&profile), std::time::Instant::now())
                .is_empty(),
            "用户已停止的服务不得被保活复活"
        );
    }

    #[test]
    fn refresh_does_not_cleanup_a_running_runtime_that_is_listening() {
        let mut runtime = entry(RuntimePhase::Running, Some(std::time::Instant::now()));
        assert!(!should_mark_runtime_error(&mut runtime, true));
    }

    #[test]
    fn refresh_does_not_cleanup_a_starting_runtime() {
        let mut runtime = entry(RuntimePhase::Starting, None);
        assert!(!should_mark_runtime_error(&mut runtime, false));
    }

    #[test]
    fn refresh_cleans_up_only_after_running_runtime_is_confirmed_missing() {
        let mut runtime = entry(
            RuntimePhase::Running,
            Some(std::time::Instant::now() - Duration::from_secs(1)),
        );
        assert!(!should_mark_runtime_error(&mut runtime, false));
        assert!(!should_mark_runtime_error(&mut runtime, false));
        assert!(should_mark_runtime_error(&mut runtime, false));
    }

    #[test]
    fn a_recovered_port_clears_missing_port_checks() {
        let mut runtime = entry(
            RuntimePhase::Running,
            Some(std::time::Instant::now() - Duration::from_secs(1)),
        );
        assert!(!should_mark_runtime_error(&mut runtime, false));
        assert!(!should_mark_runtime_error(&mut runtime, true));
        assert!(!should_mark_runtime_error(&mut runtime, false));
    }

    #[test]
    fn upstream_preflight_failure_is_visible_as_runtime_error() {
        let profile = WorkspaceProfile::new("C:/workspace/test".into(), Some("test".into()));
        let mut supervisor = RuntimeSupervisor::default();

        supervisor.set_mcp_error(&profile, "本地 MCP 上游初始化失败：连接超时".into());

        let status = supervisor.mcp_status(&profile);
        assert_eq!(status.state, "error");
        assert!(status.local_message.contains("上游初始化失败"));
    }
}
