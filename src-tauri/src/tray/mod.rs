//! 系统托盘：状态图标、状态菜单行，以及“开机自启并启动 MCP/隧道”。
//!
//! 图标颜色由 [`state`] 聚合出的实际状态决定；状态行文字与 tooltip 同时给出
//! MCP 与隧道信息，避免只靠颜色传达状态。

pub mod autostart;
pub mod icon;
pub mod state;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::app_state::AppState;
use crate::tunnel::TunnelServiceKind;
use crate::workspace::WorkspaceProfile;

use icon::Level;
use state::{ServiceSample, TrayState, WorkspaceSample};

/// 托盘图标 id。
pub const TRAY_ID: &str = "main-tray";

const ID_STATUS: &str = "tray-status";
const ID_NOTICE: &str = "tray-notice";
const ID_SHOW: &str = "tray-show";
const ID_AUTOSTART: &str = "tray-autostart";
const ID_QUIT: &str = "tray-quit";

/// 状态轮询间隔。串行执行，避免重入。
const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// 菜单句柄，用于原地更新文字与勾选状态，避免每 5 秒重建整个菜单。
static STATUS_ITEM: OnceLock<MenuItem<Wry>> = OnceLock::new();
static NOTICE_ITEM: OnceLock<MenuItem<Wry>> = OnceLock::new();
static AUTOSTART_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();

/// 最近一次聚合结果，用于避免无变化时重复设置图标。
static LAST_STATE: Mutex<Option<TrayState>> = Mutex::new(None);

/// 构建托盘菜单并挂载事件。
pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, ID_STATUS, "状态：采样中…", false, None::<&str>)?;
    // 专用于展示失败/提示的只读行；无提示时文字为空，不干扰状态行。
    let notice = MenuItem::with_id(app, ID_NOTICE, "", false, None::<&str>)?;
    let show = MenuItem::with_id(app, ID_SHOW, "显示窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "退出", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let separator2 = PredefinedMenuItem::separator(app)?;

    let _ = STATUS_ITEM.set(status.clone());
    let _ = NOTICE_ITEM.set(notice.clone());

    // 非 Windows 平台没有 HKCU Run，自启无法生效；此时不展示该菜单项，
    // 避免给用户一个必然失败的操作。
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> =
        vec![&status, &notice, &separator, &show];
    if autostart::is_supported() {
        let autostart_item = CheckMenuItem::with_id(
            app,
            ID_AUTOSTART,
            autostart_label(app.handle()),
            true,
            autostart::is_enabled(),
            None::<&str>,
        )?;
        // 存入静态存储，取回 'static 引用供菜单使用。
        let _ = AUTOSTART_ITEM.set(autostart_item);
        if let Some(item) = AUTOSTART_ITEM.get() {
            items.push(item);
        }
    }
    items.push(&separator2);
    items.push(&quit);

    let menu = Menu::with_items(app, &items)?;

    TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Coding Tools MCP")
        // 右键菜单，左键显示窗口。
        .show_menu_on_left_click(false)
        .icon(icon::image_for(Level::Stopped))
        .on_menu_event(|app, event| match event.id.as_ref() {
            ID_SHOW => {
                // 用户已看到窗口，清除陈旧提示。
                clear_notice();
                let _ = crate::commands::window_chrome::show_main_window(app.clone());
            }
            ID_AUTOSTART => on_toggle_autostart(app),
            ID_QUIT => {
                crate::commands::window_chrome::arm_allow_exit();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = crate::commands::window_chrome::show_main_window(tray.app_handle().clone());
            }
        })
        .build(app)?;
    Ok(())
}

/// 启动 5 秒轮询。串行执行，避免重入。
pub fn spawn_refresh_loop(app: AppHandle) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        // 首次立即采样，保证托盘首帧就落到正确颜色。
        refresh(&app).await;
        loop {
            tokio::time::sleep(REFRESH_INTERVAL).await;
            refresh(&app).await;
        }
    });
}

/// 采样一次并应用。
pub async fn refresh(app: &AppHandle) {
    let sampled = sample(app).await;
    apply(app, &sampled);
}

/// 采样所有工作区的实际 MCP 与隧道状态。
async fn sample(app: &AppHandle) -> TrayState {
    let state = app.state::<AppState>();
    let profiles: Vec<WorkspaceProfile> = state
        .with_workspaces(|store| Ok(store.list().to_vec()))
        .unwrap_or_default();

    // 运行时阶段：refresh_mcp 会校验监听端口是否真的在跑。
    let phases: Vec<ServiceSample> = profiles.iter().map(|p| phase_of(&state, p)).collect();

    // 隧道：以监督器里的实际会话/进程存活为准，而不是保存过的公网 URL。
    let settings = state::load_settings();
    let tunnels: Vec<(bool, bool)> = {
        let guard = crate::tunnel::supervisor().lock().await;
        profiles
            .iter()
            .map(|profile| {
                let configured = state::mcp_tunnel_configured(profile);
                let online = if configured {
                    guard
                        .status(profile, TunnelServiceKind::Mcp, &settings)
                        .state
                        == "running"
                } else {
                    false
                };
                (configured, online)
            })
            .collect()
    };

    let samples: Vec<WorkspaceSample> = profiles
        .iter()
        .zip(phases)
        .zip(tunnels)
        .map(|((profile, phase), (configured, online))| WorkspaceSample {
            name: profile.name.clone(),
            mcp: phase,
            tunnel_configured: configured,
            tunnel_online: online,
        })
        .collect();

    state::aggregate(samples)
}

/// 读取单个工作区的 MCP 实际阶段。
fn phase_of(state: &AppState, profile: &WorkspaceProfile) -> ServiceSample {
    state
        .with_runtime(|runtime| {
            runtime.refresh_mcp(profile);
            Ok(runtime.mcp_status(profile).state)
        })
        .ok()
        .map(|phase| match phase.as_str() {
            "running" => ServiceSample::Healthy,
            "starting" | "stopping" => ServiceSample::Transitioning,
            // MCP 自身错误与隧道离线是两回事，不能混为一谈。
            "error" => ServiceSample::McpError,
            _ => ServiceSample::Stopped,
        })
        .unwrap_or(ServiceSample::Stopped)
}

/// 把状态应用到图标、tooltip 与菜单。
fn apply(app: &AppHandle, sampled: &TrayState) {
    let previous = LAST_STATE
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    if let Ok(mut guard) = LAST_STATE.lock() {
        *guard = Some(sampled.clone());
    }

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        // 只在档位变化时替换图标，避免每 5 秒无谓重绘与托盘闪烁。
        let level_changed = previous.as_ref().map(|p| p.level) != Some(sampled.level);
        if level_changed {
            let _ = tray.set_icon(Some(icon::image_for(sampled.level)));
        }
        // tooltip 仅在实际内容变化时更新。
        if previous.as_ref().map(|p| p.tooltip.as_str()) != Some(sampled.tooltip.as_str()) {
            let _ = tray.set_tooltip(Some(sampled.tooltip.clone()));
        }
    }

    if previous.as_ref().map(|p| p.summary.as_str()) != Some(sampled.summary.as_str()) {
        if let Some(item) = STATUS_ITEM.get() {
            let _ = item.set_text(format!("状态：{}", sampled.summary));
        }
    }
    sync_autostart_item(app);
}

/// 让勾选状态与文案反映真实注册表与绑定工作区。
fn sync_autostart_item(app: &AppHandle) {
    if let Some(item) = AUTOSTART_ITEM.get() {
        let _ = item.set_checked(autostart::is_enabled());
        let _ = item.set_text(autostart_label(app));
    }
}

/// 自启菜单项文案：开启后明确显示绑定的工作区名称。
fn autostart_label(app: &AppHandle) -> String {
    const BASE: &str = "开机自启并启动 MCP/隧道";
    let state = app.state::<AppState>();
    let target = state
        .with_workspaces(|store| Ok(store.autostart_launch_target()))
        .unwrap_or(crate::data::AutostartTarget::Unbound);
    match target {
        crate::data::AutostartTarget::Bound(id) => {
            let name = state
                .with_workspaces(|store| Ok(store.get(&id).map(|p| p.name.clone())))
                .ok()
                .flatten()
                .unwrap_or(id);
            format!("{BASE}（{name}）")
        }
        // 绑定已失效时必须明确告知，而不是回退显示成另一个工作区。
        crate::data::AutostartTarget::Missing(_) => format!("{BASE}（目标已失效）"),
        crate::data::AutostartTarget::Unbound => BASE.to_string(),
    }
}

/// 勾选/取消“开机自启并启动 MCP/隧道”。
fn on_toggle_autostart(app: &AppHandle) {
    let state = app.state::<AppState>();
    let ops = LiveOps { state: &state };
    let currently = autostart::run_state();

    if currently.is_enabled() {
        // 关闭只取消自启与绑定，不停止当前正在运行的服务。
        match autostart::disable(&ops) {
            Ok(()) => notify(app, "已关闭开机自启"),
            Err(error) => notify(app, &format!("关闭开机自启失败：{error}")),
        }
    } else {
        // 无法判定注册表状态时不冒险修改。
        if let autostart::RunState::Unknown(reason) = currently {
            notify(app, &format!("无法读取开机自启状态，已取消操作：{reason}"));
            sync_autostart_item(app);
            return;
        }

        // 绑定只用用户明确选中的工作区，不回退到首个工作区。
        let selected = state
            .with_workspaces(|store| Ok(store.selected_workspace_id()))
            .unwrap_or_default();
        if selected.trim().is_empty() {
            notify(app, "请先在界面中打开一个工作区，再开启开机自启");
            sync_autostart_item(app);
            return;
        }
        let name = state
            .with_workspaces(|store| Ok(store.get(&selected).map(|p| p.name.clone())))
            .ok()
            .flatten()
            .unwrap_or_else(|| selected.clone());

        match autostart::enable(&ops, &selected) {
            Ok(()) => notify(app, &format!("已开启开机自启，将启动「{name}」的 MCP")),
            Err(error) => notify(app, &format!("开启开机自启失败：{error}")),
        }
    }

    // 与健康状态无关，必须显式刷新勾选与文案。
    sync_autostart_item(app);
}

/// 把真实注册表与数据存储接到 [`autostart::AutostartOps`]。
struct LiveOps<'a> {
    state: &'a AppState,
}

impl autostart::AutostartOps for LiveOps<'_> {
    fn read_run(&self) -> Result<Option<String>, String> {
        autostart::read_run_value()
    }

    fn write_run(&self, value: Option<&str>) -> Result<(), String> {
        autostart::write_run_value(value)
    }

    fn read_binding(&self) -> Result<String, String> {
        // 读取失败必须上报：吞掉错误会让回滚丢掉旧目标。
        self.state
            .with_workspaces(|store| Ok(store.autostart_workspace_id()))
            .map_err(|error| error.to_string())
    }

    fn write_binding(&self, value: &str, restore: bool) -> Result<(), String> {
        self.state
            .with_workspaces(|store| {
                if restore {
                    // 回滚：旧值可能指向已删除的工作区，不做存在性校验。
                    store.restore_autostart_workspace(value)
                } else if value.trim().is_empty() {
                    store.clear_autostart_workspace()
                } else {
                    store.set_autostart_workspace(value)
                }
            })
            .map_err(|error| error.to_string())
    }
}

/// 提示用户。
///
/// tooltip 会被下一轮刷新覆盖，release 下 stderr 也不可见，因此写进一个**专用只读
/// 菜单行**：它不被状态轮询覆盖，用户右键就能看到失败原因。
fn notify(app: &AppHandle, message: &str) {
    if let Some(item) = NOTICE_ITEM.get() {
        let _ = item.set_text(format!("提示：{message}"));
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(format!("Coding Tools MCP · {message}")));
    }
    eprintln!("[tray] {message}");
}

/// 用户确认后清除提示行。
pub fn clear_notice() {
    if let Some(item) = NOTICE_ITEM.get() {
        let _ = item.set_text("");
    }
}

/// 登录自启路径：后台启动绑定工作区的 MCP（并复用隧道联动）。
///
/// **不回退**：未绑定或绑定工作区已被删除时明确报错并停止，
/// 绝不静默改启动另一个工作区。
pub async fn run_autostart_startup(app: AppHandle) {
    let state = app.state::<AppState>();
    let target = state
        .with_workspaces(|store| Ok(store.autostart_launch_target()))
        .unwrap_or(crate::data::AutostartTarget::Unbound);

    let (id, name) = match target {
        crate::data::AutostartTarget::Bound(id) => {
            let name = state
                .with_workspaces(|store| Ok(store.get(&id).map(|p| p.name.clone())))
                .ok()
                .flatten()
                .unwrap_or_else(|| id.clone());
            (id, name)
        }
        crate::data::AutostartTarget::Unbound => {
            notify(&app, "开机自启未绑定工作区，已跳过启动");
            refresh(&app).await;
            return;
        }
        crate::data::AutostartTarget::Missing(_) => {
            notify(&app, "开机自启绑定的工作区已不存在，已跳过启动（未启动其它工作区）");
            refresh(&app).await;
            return;
        }
    };

    match crate::commands::runtime::start_mcp_by_id(&state, &id).await {
        Ok(status) if status.state == "running" => {
            // 启动成功不等于隧道已连：隧道错误在 start_mcp_service 中被吞掉，
            // 因此必须回查实际监督状态，避免把失败报成全部成功。
            match tunnel_online(&state, &id).await {
                Some(true) => notify(&app, &format!("开机自启已启动「{name}」的 MCP 与隧道")),
                Some(false) => notify(
                    &app,
                    &format!("「{name}」的 MCP 已启动，但隧道未能连接（已配置但未在线）"),
                ),
                None => notify(&app, &format!("「{name}」的 MCP 已启动（未配置隧道）")),
            }
        }
        Ok(status) => notify(
            &app,
            &format!("开机自启启动「{name}」的 MCP 未成功：{}", status.local_message),
        ),
        Err(error) => notify(&app, &format!("开机自启启动「{name}」的 MCP 失败：{error}")),
    }
    refresh(&app).await;
}

/// 回查某工作区的 MCP 隧道实际是否在线。
///
/// 返回 `None` 表示该工作区未配置隧道。注意：这里只验证本机隧道进程/会话存活，
/// 不等同于公网端到端可达。
async fn tunnel_online(state: &AppState, id: &str) -> Option<bool> {
    let profile = state
        .with_workspaces(|store| Ok(store.get(id).cloned()))
        .ok()
        .flatten()?;
    if !state::mcp_tunnel_configured(&profile) {
        return None;
    }
    let settings = state::load_settings();
    let guard = crate::tunnel::supervisor().lock().await;
    Some(guard.status(&profile, TunnelServiceKind::Mcp, &settings).state == "running")
}

/// 便于测试与复用的图标构造。
#[allow(dead_code)]
pub fn icon_image(level: Level) -> Image<'static> {
    icon::image_for(level)
}
