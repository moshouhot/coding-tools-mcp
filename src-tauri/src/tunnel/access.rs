use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::LazyLock;

use std::collections::HashSet;

use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

use crate::data::DataStore;
use crate::error::AppResult;
use crate::platform::platform;
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

use super::frp;
use super::{TunnelServiceKind, TunnelSupervisor};

static TUNNEL_SUPERVISOR: LazyLock<Mutex<TunnelSupervisor>> =
    LazyLock::new(|| Mutex::new(TunnelSupervisor::new()));
static FRP_HEALTH_LOOP_STARTED: AtomicBool = AtomicBool::new(false);
/// True only after a sustained offline window; avoids thrashing on probe flaps.
static HOST_NETWORK_MARKED_OFFLINE: AtomicBool = AtomicBool::new(false);
static HOST_NETWORK_OFFLINE_STREAK: AtomicU32 = AtomicU32::new(0);

const FRP_HEALTH_INTERVAL: Duration = Duration::from_secs(15);
const HOST_OFFLINE_STREAK_TO_MARK: u32 = 2;

pub fn supervisor() -> &'static Mutex<TunnelSupervisor> {
    &TUNNEL_SUPERVISOR
}

/// 后台健康循环：修复卡住的 FRP 客户端，并探测 Cloudflare 隧道就绪。
///
/// Cloudflare 侧**只探测、不重启** —— cloudflared 自身在按退避重连，
/// 边缘不可达时反复杀进程只会在网络恢复瞬间制造重启风暴。
pub fn ensure_frp_health_loop() {
    if FRP_HEALTH_LOOP_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async {
        loop {
            sleep(FRP_HEALTH_INTERVAL).await;
            let settings = AppSettings::load_or_default();
            let online = frp::probe_host_network_available().await;
            let recovered = update_host_network_state(online);
            let mut guard = supervisor().lock().await;
            // Cloudflare 隧道：进程活着不代表在线，用 /ready 判；离线只标记不重启。
            let offline = guard.probe_cloudflare_readiness().await;
            if offline > 0 {
                crate::lifecycle::record(&format!(
                    "cloudflare 隧道有 {offline} 个会话未建立边缘连接（不自动重启，等待其自身重连）"
                ));
            }
            if recovered {
                let _ = guard
                    .restart_frpc_after_network_recovery(&settings)
                    .await;
            }
            let _ = guard.heal_unhealthy_frpc(&settings).await;
        }
    });
}

fn update_host_network_state(online: bool) -> bool {
    if online {
        HOST_NETWORK_OFFLINE_STREAK.store(0, Ordering::SeqCst);
        // Recovery only fires after we previously marked a sustained outage.
        HOST_NETWORK_MARKED_OFFLINE.swap(false, Ordering::SeqCst)
    } else {
        let streak = HOST_NETWORK_OFFLINE_STREAK.fetch_add(1, Ordering::SeqCst) + 1;
        if streak >= HOST_OFFLINE_STREAK_TO_MARK {
            HOST_NETWORK_MARKED_OFFLINE.store(true, Ordering::SeqCst);
        }
        false
    }
}

fn tunnel_type_for(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> &str {
    match kind {
        TunnelServiceKind::Mcp => profile.tunnel.tunnel_type.as_str(),
        TunnelServiceKind::Actions => profile.actions.tunnel_type.as_str(),
    }
}

pub async fn maybe_start_for_runtime(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<Option<String>> {
    let tunnel_type = tunnel_type_for(profile, kind);
    if tunnel_type.is_empty() || tunnel_type == "none" {
        return Ok(None);
    }
    let settings = AppSettings::load_or_default();
    let mut guard = supervisor().lock().await;
    let status = guard.start(profile, kind, &settings).await?;
    Ok(Some(status.public_url))
}

pub async fn stop_for_runtime(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<()> {
    let settings = AppSettings::load_or_default();
    let mut guard = supervisor().lock().await;
    guard.stop(profile, kind, &settings).await
}

pub async fn drop_workspace(workspace_id: &str) -> AppResult<()> {
    let mut guard = supervisor().lock().await;
    guard.drop_workspace(workspace_id).await
}

pub async fn sync_managed_runtime_routes(
    active_runtime_keys: HashSet<(String, TunnelServiceKind)>,
) -> AppResult<()> {
    let settings = AppSettings::load_or_default();
    let profiles = DataStore::read_file(|data| Ok(data.profiles.clone()))?;
    let mut guard = supervisor().lock().await;
    guard.restore_active_frp_routes(&profiles, &active_runtime_keys, &settings);
    Ok(())
}

pub async fn cleanup_orphan_for_runtime(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    runtime_listening: bool,
) -> AppResult<()> {
    let port = match kind {
        TunnelServiceKind::Mcp => profile.runtime.local_port,
        TunnelServiceKind::Actions => profile.actions.local_port,
    };
    if runtime_listening || platform().find_pid_listening_on_port(port)?.is_some() {
        return Ok(());
    }
    let mut guard = supervisor().lock().await;
    // 等待 supervisor 锁期间 runtime 可能已经恢复，再确认一次才允许删除 route。
    if platform().find_pid_listening_on_port(port)?.is_some() {
        return Ok(());
    }
    guard.cleanup_orphan(profile, kind, false).await
}

#[cfg(test)]
mod tests {
    use super::{
        update_host_network_state, HOST_NETWORK_MARKED_OFFLINE, HOST_NETWORK_OFFLINE_STREAK,
        HOST_OFFLINE_STREAK_TO_MARK,
    };
    use std::sync::atomic::Ordering;

    fn reset_network_state() {
        HOST_NETWORK_MARKED_OFFLINE.store(false, Ordering::SeqCst);
        HOST_NETWORK_OFFLINE_STREAK.store(0, Ordering::SeqCst);
    }

    #[test]
    fn network_recovery_requires_sustained_offline() {
        reset_network_state();
        assert!(!update_host_network_state(false));
        assert!(!HOST_NETWORK_MARKED_OFFLINE.load(Ordering::SeqCst));
        for _ in 1..HOST_OFFLINE_STREAK_TO_MARK {
            assert!(!update_host_network_state(false));
        }
        assert!(HOST_NETWORK_MARKED_OFFLINE.load(Ordering::SeqCst));
        assert!(update_host_network_state(true));
        assert!(!HOST_NETWORK_MARKED_OFFLINE.load(Ordering::SeqCst));
        // Online flaps without a sustained outage must not restart.
        assert!(!update_host_network_state(true));
    }

    #[test]
    fn single_offline_probe_does_not_mark_outage() {
        reset_network_state();
        assert!(!update_host_network_state(false));
        assert!(!HOST_NETWORK_MARKED_OFFLINE.load(Ordering::SeqCst));
        // Brief flap back online must not count as recovery.
        assert!(!update_host_network_state(true));
    }
}
