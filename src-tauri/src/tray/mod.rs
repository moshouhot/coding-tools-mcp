//! 系统托盘：状态图标、状态菜单行，以及“开机自启并启动 MCP/隧道”。
//!
//! 图标只有两态：有 MCP 在监听时用原图标，否则用灰度图标；状态行文字与
//! tooltip 给出 MCP 与隧道细节，避免只靠颜色传达状态。失败通过系统对话框提示。

pub mod autostart;
pub mod icon;
pub mod state;

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use tokio::sync::Mutex as AsyncMutex;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

use crate::app_state::AppState;
use crate::tunnel::TunnelServiceKind;
use crate::workspace::WorkspaceProfile;

use icon::Level;
use state::{ServiceSample, TrayState, WorkspaceSample};

/// 托盘图标 id。
pub const TRAY_ID: &str = "main-tray";

const ID_STATUS: &str = "tray-status";
const ID_SHOW: &str = "tray-show";
const ID_AUTOSTART: &str = "tray-autostart";
const ID_QUIT: &str = "tray-quit";

/// 状态轮询间隔。串行执行，避免重入。
const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// 菜单句柄，用于原地更新文字与勾选状态，避免每 5 秒重建整个菜单。
static STATUS_ITEM: OnceLock<MenuItem<Wry>> = OnceLock::new();
static AUTOSTART_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();

/// 最近一次聚合结果，用于避免无变化时重复设置图标。
static LAST_STATE: Mutex<Option<TrayState>> = Mutex::new(None);

/// 启动恢复期间暂停「运行中集合」的记录（**深度计数**）。
///
/// 首轮采样发生在恢复开始之前，此时什么都还没启动；若不暂停，会把「当前为空」
/// 写回磁盘，待恢复集合在恢复动作真正开始前就被抹掉。
///
/// 用计数而非布尔：启动恢复与内部重启可能嵌套（用户在恢复窗口内点了重启），
/// 内层结束时不得把外层的暂停一并解除。
static SUPPRESS_DEPTH: AtomicU32 = AtomicU32::new(0);

/// 记录代次。启动动作改变暂停状态时递增。
///
/// 采样会先读 MCP 相位、再 `await` 隧道锁；若这个间隙里发生了一次启动转换，
/// 那个样本已经过期，用它落盘会覆盖刚恢复好的集合。采样前先记下代次，
/// 落盘前对比，不一致则丢弃本次记录（下一轮轮询会重新采）。
static RECORD_GENERATION: AtomicU64 = AtomicU64::new(0);

/// 串行化「观测 → 落盘」，避免并发 refresh 以旧快照覆盖新快照。
static RECORD_GATE: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

/// 记录失败是否已报告过（按失败片段去重，避免每 5 秒弹一次）。
static RECORD_FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);

/// 全局 AppHandle，供运行时转换（启动/停止/重启）同步记录状态使用。
///
/// `commands/runtime.rs` 只拿得到 `&AppState`，拿不到 `AppHandle`，
/// 因此需要在这里保存一份，才能把转换结果落盘。
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// 构建托盘菜单并挂载事件。
pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, ID_STATUS, "状态：采样中…", false, None::<&str>)?;
    let show = MenuItem::with_id(app, ID_SHOW, "显示窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "退出", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let separator2 = PredefinedMenuItem::separator(app)?;

    let _ = STATUS_ITEM.set(status.clone());

    // 非 Windows 平台没有 HKCU Run，自启无法生效；此时不展示该菜单项，
    // 避免给用户一个必然失败的操作。
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![&status, &separator, &show];
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
        .icon(icon::image_for(app.handle(), Level::Stopped))
        .on_menu_event(|app, event| match event.id.as_ref() {
            ID_SHOW => {
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
    // 供运行时转换路径同步记录状态。
    let _ = APP_HANDLE.set(app.handle().clone());
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
    // 先记下代次：相位是在 await 隧道锁之前采的，若这个间隙里发生了
    // 启动转换，这个样本已过期，不得用来落盘。
    let generation = RECORD_GENERATION.load(Ordering::SeqCst);
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
            id: profile.id.clone(),
            name: profile.name.clone(),
            mcp: phase,
            tunnel_configured: configured,
            tunnel_online: online,
        })
        .collect();

    // 记录「确实在监听」的集合。注意：只把 MCP 相位作为记录依据，
    // 不使用受隧道采样延迟影响的字段。
    let running: Vec<String> = samples
        .iter()
        .filter(|sample| state::mcp_listening(sample))
        .map(|sample| sample.id.clone())
        .collect();
    record_running_mcp(app, &running, generation).await;

    state::aggregate(samples)
}

/// 立即根据当前监督器状态记录运行中的 MCP。
///
/// **严格只读**：不调 `refresh_mcp`（它会改监督器状态并在失效时 spawn 清理任务），
/// 只读相位 + 实际端口监听，因此可在退出回调里安全调用（退出阶段 spawn 可能 panic）。
///
/// 用途：轮询最多有 5 秒延迟，若用户刚停止 MCP 就退出，磁盘上会留着陈旧的
/// 「运行中」值；退出时补一次快照可消除这个窗口。进程退出不会主动停服务，
/// 所以此刻端口仍在监听，采样结果是真实的最终状态。
/// 记录当前实际在运行的 MCP。
///
/// **严格只读**：不调 `refresh_mcp`（它会改监督器状态并在失效时 spawn 清理任务），
/// 只读相位 + 实际端口监听，因此可在退出回调里安全调用。
///
/// `bump_generation=true` 时递增代次，让在途的轮询样本失效（真实转换发生时用），
/// 避免旧快照覆盖本次转换的结果。
///
/// 任何读取失败都**保留原有记录**而不是当作空集合——未知状态不得抹掉历史。
fn record_current_state_inner(app: &AppHandle, bump_generation: bool) {
    if recording_suppressed() {
        return;
    }
    if bump_generation {
        RECORD_GENERATION.fetch_add(1, Ordering::SeqCst);
    }
    let state = app.state::<AppState>();
    let Ok(profiles) = state.with_workspaces(|store| Ok(store.list().to_vec())) else {
        eprintln!("[tray] 读取工作区失败，保留原有运行状态记录");
        return;
    };

    let mut running: Vec<String> = Vec::new();
    for profile in &profiles {
        // 相位必须为 running，且端口确实在监听：与轮询口径一致，但不改状态。
        let phase = match state.with_runtime(|runtime| Ok(runtime.mcp_status(profile).state)) {
            Ok(phase) => phase,
            Err(error) => {
                eprintln!("[tray] 读取运行时状态失败，保留原有记录：{error}");
                return;
            }
        };
        if phase != "running" {
            continue;
        }
        match crate::platform::platform().find_pid_listening_on_port(profile.runtime.local_port) {
            Ok(Some(_)) => running.push(profile.id.clone()),
            Ok(None) => {}
            Err(error) => {
                eprintln!("[tray] 检查端口失败，保留原有记录：{error}");
                return;
            }
        }
    }

    if let Err(error) = state.with_workspaces(|store| {
        store.set_running_mcp_workspace_ids(&running).map(|_| ())
    }) {
        // 退出阶段不弹对话框（会阻断退出），但必须留下痕迹。
        eprintln!("[tray] 记录运行状态失败：{error}");
    }
}

/// 退出时补一次快照（不改代次，避免影响在途轮询）。
///
/// 退出阶段应用即将结束，无并发写入，因此不取串行锁。
pub fn record_running_now(app: &AppHandle) {
    record_current_state_inner(app, false);
}

/// 真实转换（启动/停止/重启）完成后立即记录。
///
/// 供 `commands/runtime.rs` 调用：轮询最多有 5 秒延迟，若在这窗口内退出，
/// 磁盘上会留着陈旧值；转换后立刻记录可消除这个窗口。
///
/// 与轮询共用串行锁，避免两者以旧快照互相覆盖；递增代次使转换前
/// 开始的轮询样本失效。
pub async fn record_current_state_async() {
    let Some(app) = APP_HANDLE.get() else {
        return;
    };
    let _guard = RECORD_GATE.lock().await;
    record_current_state_inner(app, true);
}

/// 纯函数：本次记录是否应跳过。
///
/// 两种情况：
///
/// - 启动动作期间被暂停（`suppressed`）；
/// - 采样代次与当前代次不一致——说明这个样本是在某次启动转换**之前**开始采集的，
///   已经过期，用它落盘会覆盖刚恢复好的集合（延迟启动样本竞态）。
fn should_skip_record(suppressed: bool, sample_generation: u64, current_generation: u64) -> bool {
    suppressed || sample_generation != current_generation
}

/// 一次 MCP 启动尝试的结果类别。
///
/// `Ok(...)` **不代表启动成功**：状态可能是 `error`/`stopped`，必须看 `state`。
enum McpStartOutcome {
    /// 确实在监听。
    Running,
    /// 未成功（带可读原因）。
    Failed(String),
}

/// 纯函数：把启动返回值归类。
fn classify_mcp_start(result: &crate::error::AppResult<crate::workspace::RuntimeStatusDto>) -> McpStartOutcome {
    match result {
        Ok(status) if status.state == "running" => McpStartOutcome::Running,
        Ok(status) => McpStartOutcome::Failed(format!(
            "状态 {}：{}",
            status.state, status.local_message
        )),
        Err(error) => McpStartOutcome::Failed(error.to_string()),
    }
}

/// 记录当前**实际**在运行的 MCP 工作区，供下次普通启动恢复。
///
/// 记录的是真实观察到的监听状态，而不是「用户点过启动」的意图。
///
/// `generation` 是采样开始时的代次；若其间发生了启动转换（代次已变），
/// 说明这个样本已过期，直接丢弃，避免用旧快照覆盖刚恢复好的集合。
async fn record_running_mcp(app: &AppHandle, running: &[String], generation: u64) {
    if recording_suppressed() {
        return;
    }
    // 串行化落盘，避免并发 refresh 以旧快照覆盖新快照。
    let _guard = RECORD_GATE.lock().await;
    if should_skip_record(
        recording_suppressed(),
        generation,
        RECORD_GENERATION.load(Ordering::SeqCst),
    ) {
        return;
    }

    let state = app.state::<AppState>();
    // 读失败不得被当成「空集合」：未知状态不能抹掉历史。
    let saved = state.with_workspaces(|store| {
        store
            .set_running_mcp_workspace_ids(running)
            .map(|_| ())
    });

    match saved {
        Ok(()) => {
            // 成功后重置，下次失败会重新报告一次。
            RECORD_FAILURE_REPORTED.store(false, Ordering::SeqCst);
        }
        Err(error) => {
            // 每个失败片段只报告一次，避免每 5 秒弹一个对话框。
            if !RECORD_FAILURE_REPORTED.swap(true, Ordering::SeqCst) {
                report_failure(
                    app,
                    &format!("记录运行状态失败（下次启动可能无法恢复服务）：{error}"),
                );
            } else {
                eprintln!("[tray] 记录运行状态仍失败：{error}");
            }
        }
    }
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
            let _ = tray.set_icon(Some(icon::image_for(app, sampled.level)));
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

/// 自启菜单项文案。
///
/// 只有一个工作区时名称是冗余的，省略；多工作区时才标出绑定的工作区名，
/// 避免用户搞不清登录时会启动哪一个。
///
/// 绑定已失效时必须明确告知，而不是回退显示成另一个工作区。
fn autostart_label_text(
    workspace_count: usize,
    target: &crate::data::AutostartTarget,
    bound_name: Option<&str>,
) -> String {
    const BASE: &str = "开机自启并启动 MCP/隧道";
    match target {
        crate::data::AutostartTarget::Bound(_) if workspace_count > 1 => {
            let name = bound_name.unwrap_or("未知工作区");
            format!("{BASE}（{name}）")
        }
        crate::data::AutostartTarget::Missing(_) => format!("{BASE}（目标已失效）"),
        _ => BASE.to_string(),
    }
}

/// 读取当前状态并生成自启菜单项文案。
fn autostart_label(app: &AppHandle) -> String {
    let state = app.state::<AppState>();
    let info = state.with_workspaces(|store| {
        let count = store.list().len();
        let target = store.autostart_launch_target();
        let name = match &target {
            crate::data::AutostartTarget::Bound(id) => store.get(id).map(|p| p.name.clone()),
            _ => None,
        };
        Ok((count, target, name))
    });

    match info {
        Ok((count, target, name)) => autostart_label_text(count, &target, name.as_deref()),
        // 读不到状态时不要假装已绑定：退回到中性文案。
        Err(_) => "开机自启并启动 MCP/隧道".to_string(),
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
            Ok(()) => report_ok("已关闭开机自启"),
            Err(error) => report_failure(app, &format!("关闭开机自启失败：{error}")),
        }
    } else {
        // 无法判定注册表状态时不冒险修改。
        if let autostart::RunState::Unknown(reason) = currently {
            report_failure(app, &format!("无法读取开机自启状态，已取消操作：{reason}"));
            sync_autostart_item(app);
            return;
        }

        // 绑定只用用户明确选中的工作区，不回退到首个工作区。
        let selected = state
            .with_workspaces(|store| Ok(store.selected_workspace_id()))
            .unwrap_or_default();
        if selected.trim().is_empty() {
            report_failure(app, "请先在界面中打开一个工作区，再开启开机自启");
            sync_autostart_item(app);
            return;
        }
        let name = state
            .with_workspaces(|store| Ok(store.get(&selected).map(|p| p.name.clone())))
            .ok()
            .flatten()
            .unwrap_or_else(|| selected.clone());

        match autostart::enable(&ops, &selected) {
            Ok(()) => report_ok(&format!("已开启开机自启，将启动「{name}」的 MCP")),
            Err(error) => report_failure(app, &format!("开启开机自启失败：{error}")),
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

/// 报告失败：写日志并弹一个系统对话框。
///
/// 托盘菜单里不再有「提示」行，tooltip 又会被下一轮 5 秒刷新覆盖，
/// release 下 stderr 也不可见，因此失败必须走用户确实能看到的出口。
fn report_failure(app: &AppHandle, message: &str) {
    eprintln!("[tray] {message}");
    app.dialog()
        .message(message)
        .title("Coding Tools MCP")
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

/// 报告成功：只记日志。
///
/// 成功不需要打断用户：菜单勾选、状态行与图标已经反映了结果。
fn report_ok(message: &str) {
    eprintln!("[tray] {message}");
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
            report_failure(&app, "开机自启未绑定工作区，已跳过启动");
            refresh(&app).await;
            return;
        }
        crate::data::AutostartTarget::Missing(_) => {
            report_failure(
                &app,
                "开机自启绑定的工作区已不存在，已跳过启动（未启动其它工作区）",
            );
            refresh(&app).await;
            return;
        }
    };

    match crate::commands::runtime::start_mcp_by_id(&state, &id).await {
        Ok(status) if status.state == "running" => {
            // 启动成功不等于隧道已连：隧道错误在 start_mcp_service 中被吞掉，
            // 因此必须回查实际监督状态，避免把失败报成全部成功。
            match tunnel_online(&state, &id).await {
                Some(true) => report_ok(&format!("开机自启已启动「{name}」的 MCP 与隧道")),
                Some(false) => report_failure(
                    &app,
                    &format!("「{name}」的 MCP 已启动，但隧道未能连接（已配置但未在线）"),
                ),
                None => report_ok(&format!("「{name}」的 MCP 已启动（未配置隧道）")),
            }
        }
        Ok(status) => report_failure(
            &app,
            &format!("开机自启启动「{name}」的 MCP 未成功：{}", status.local_message),
        ),
        Err(error) => report_failure(&app, &format!("开机自启启动「{name}」的 MCP 失败：{error}")),
    }
    refresh(&app).await;
}

/// 启动路径。两条路径**互斥**，绝不同时生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPath {
    /// 登录自启（`--autostart`）：只启动绑定的那个工作区，不读历史运行集合。
    Autostart,
    /// 普通启动：恢复上次实际在运行的 MCP 集合，不读自启绑定。
    Restore,
}

/// 纯函数：根据是否 `--autostart` 调用选择启动路径。
///
/// 抽成纯函数是为了能直接断言两条路径的互斥性，避免以后被改成一个
/// 「两个都跑」的分支。
pub fn startup_path(is_autostart_invocation: bool) -> StartupPath {
    if is_autostart_invocation {
        StartupPath::Autostart
    } else {
        StartupPath::Restore
    }
}

/// 暂停 / 恢复「运行中集合」的记录。
///
/// 启动动作（登录自启或恢复）期间必须暂停：首轮采样发生在服务真正拉起之前，
/// 若不暂停就会把「当前为空」写回磁盘，待恢复集合在恢复动作开始前就被抹掉。
pub fn suppress_recording(suppress: bool) {
    if suppress {
        SUPPRESS_DEPTH.fetch_add(1, Ordering::SeqCst);
    } else {
        // 饱和减：避免不平衡的调用把计数减到负数（会让暂停永久失效）。
        let _ = SUPPRESS_DEPTH.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |d| {
            Some(d.saturating_sub(1))
        });
    }
    // 递增代次：让「启动转换之前开始、之后才完成」的过期样本失效，
    // 否则它会用启动前的旧快照覆盖刚恢复好的集合。
    RECORD_GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// 当前是否处于暂停记录状态。
fn recording_suppressed() -> bool {
    SUPPRESS_DEPTH.load(Ordering::SeqCst) > 0
}

/// 启动动作结束后恢复记录，并立即重采样一次。
///
/// 先重采样再记录，保证磁盘上是**启动完成后**的真实状态，而不是中间态。
pub async fn resume_recording_and_refresh(app: &AppHandle) {
    suppress_recording(false);
    refresh(app).await;
}

/// 普通启动时恢复上次实际在运行的 MCP。
///
/// 只启动上次**确实在监听**的工作区，且**绝不回退**到其它工作区；
/// 已被删除的 id 会被 `restorable_mcp_workspace_ids` 丢弃。
/// 逐个启动，失败汇总成一次可见提示，避免多工作区时弹出多个对话框。
pub async fn run_restore_startup(app: AppHandle) {
    let state = app.state::<AppState>();
    // 读失败不得被当成「没有要恢复的服务」：否则会静默不恢复。
    let plan = state.with_workspaces(|store| {
        Ok((
            store.restorable_mcp_workspace_ids(),
            store.missing_running_mcp_workspace_ids(),
        ))
    });
    let (ids, missing) = match plan {
        Ok(plan) => plan,
        Err(error) => {
            report_failure(&app, &format!("读取上次运行状态失败，本次未恢复任何服务：{error}"));
            return;
        }
    };

    let mut started: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    // 上次运行的工作区已被删除：必须显式报告，不能静默丢弃。
    for id in &missing {
        problems.push(format!("工作区已不存在（已跳过，未启动其它工作区）：{id}"));
    }

    for id in &ids {
        let name = state
            .with_workspaces(|store| Ok(store.get(id).map(|p| p.name.clone())))
            .ok()
            .flatten()
            .unwrap_or_else(|| id.clone());

        let result = crate::commands::runtime::start_mcp_by_id(&state, id).await;
        match classify_mcp_start(&result) {
            // 启动成功不等于隧道已连：隧道错误在 start_mcp_service 中被吞掉，
            // 因此必须回查实际监督状态，避免把失败报成全部成功。
            McpStartOutcome::Running => match tunnel_online(&state, id).await {
                Some(true) => started.push(format!("「{name}」MCP 与隧道")),
                Some(false) => {
                    started.push(format!("「{name}」MCP"));
                    problems.push(format!("「{name}」隧道未能连接（已配置但未在线）"));
                }
                None => started.push(format!("「{name}」MCP（未配置隧道）")),
            },
            McpStartOutcome::Failed(reason) => {
                problems.push(format!("「{name}」未能启动：{reason}"))
            }
        }
    }

    if !started.is_empty() {
        report_ok(&format!("已恢复上次运行的 MCP：{}", started.join("、")));
    }
    if !problems.is_empty() {
        report_failure(
            &app,
            &format!(
                "恢复上次运行的 MCP 时部分失败：\n{}",
                problems.join("\n")
            ),
        );
    }
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
pub fn icon_image(app: &AppHandle, level: Level) -> Image<'static> {
    icon::image_for(app, level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::AutostartTarget;

    const BASE: &str = "开机自启并启动 MCP/隧道";

    #[test]
    fn single_workspace_hides_redundant_name() {
        // 只有一个工作区时名称是冗余的：不应出现括号。
        let text = autostart_label_text(1, &AutostartTarget::Bound("id-1".into()), Some("Nextcloud"));
        assert_eq!(text, BASE);
        assert!(!text.contains('（'), "单工作区不应附带工作区名：{text}");
    }

    #[test]
    fn multiple_workspaces_show_bound_name() {
        // 多工作区时必须标出绑定的是哪一个，避免歧义。
        let text = autostart_label_text(3, &AutostartTarget::Bound("id-2".into()), Some("Nextcloud"));
        assert_eq!(text, format!("{BASE}（Nextcloud）"));
    }

    #[test]
    fn missing_target_is_reported_even_with_single_workspace() {
        // 绑定失效是异常状态，必须告知，不能因为只有一个工作区就吞掉。
        let text = autostart_label_text(1, &AutostartTarget::Missing("gone".into()), None);
        assert_eq!(text, format!("{BASE}（目标已失效）"));
    }

    #[test]
    fn unbound_never_shows_parentheses() {
        for count in [1, 5] {
            assert_eq!(autostart_label_text(count, &AutostartTarget::Unbound, None), BASE);
        }
    }

    #[test]
    fn startup_paths_are_mutually_exclusive() {
        // 登录自启走 Autostart，普通启动走 Restore；绝不能两个都跑。
        assert_eq!(startup_path(true), StartupPath::Autostart);
        assert_eq!(startup_path(false), StartupPath::Restore);
        assert_ne!(startup_path(true), startup_path(false));
    }

    #[test]
    fn delayed_sample_spanning_a_transition_is_discarded() {
        // 延迟启动样本竞态：采样在转换前开始（代次 7），转换后（代次 8）才落盘。
        // 必须丢弃，否则会用启动前的旧快照覆盖刚恢复好的集合。
        assert!(
            should_skip_record(false, 7, 8),
            "跨越启动转换的过期样本必须丢弃"
        );
        // 代次一致 → 可以记录。
        assert!(!should_skip_record(false, 8, 8));
    }

    #[test]
    fn suppressed_recording_always_skips() {
        // 启动动作期间暂停：即使代次相同也不能记录。
        assert!(should_skip_record(true, 8, 8));
        assert!(should_skip_record(true, 7, 8));
    }

    #[test]
    fn suppression_is_depth_counted_not_boolean() {
        // 启动恢复与内部重启可能嵌套：内层结束不得解除外层的暂停。
        assert_eq!(SUPPRESS_DEPTH.load(Ordering::SeqCst), 0, "测试间不得残留");
        suppress_recording(true);
        assert!(recording_suppressed());
        suppress_recording(true);
        assert!(recording_suppressed(), "两层暂停仍应生效");
        suppress_recording(false);
        assert!(recording_suppressed(), "内层结束不得解除外层暂停");
        suppress_recording(false);
        assert!(!recording_suppressed());
        // 不平衡的多余解除不得把计数减到负数（那会让暂停永久失效）。
        suppress_recording(false);
        assert!(!recording_suppressed());
        assert_eq!(SUPPRESS_DEPTH.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ok_status_without_running_is_treated_as_failure() {
        // `Ok(...)` 不代表启动成功：state 可能是 error/stopped，必须看 state。
        let errored = Ok(crate::workspace::RuntimeStatusDto {
            state: "error".into(),
            pid: None,
            local_message: "端口被占用".into(),
            public_message: String::new(),
            local_endpoint: String::new(),
            public_endpoint: String::new(),
        });
        match classify_mcp_start(&errored) {
            McpStartOutcome::Failed(reason) => assert!(
                reason.contains("error") && reason.contains("端口被占用"),
                "失败原因必须包含状态与说明：{reason}"
            ),
            McpStartOutcome::Running => panic!("error 状态不得被当成启动成功"),
        }

        let stopped = Ok(crate::workspace::RuntimeStatusDto {
            state: "stopped".into(),
            pid: None,
            local_message: String::new(),
            public_message: String::new(),
            local_endpoint: String::new(),
            public_endpoint: String::new(),
        });
        assert!(matches!(classify_mcp_start(&stopped), McpStartOutcome::Failed(_)));

        // 只有 running 才算成功。
        let running = Ok(crate::workspace::RuntimeStatusDto {
            state: "running".into(),
            pid: Some(1),
            local_message: String::new(),
            public_message: String::new(),
            local_endpoint: String::new(),
            public_endpoint: String::new(),
        });
        assert!(matches!(classify_mcp_start(&running), McpStartOutcome::Running));

        // Err 同样算失败。
        let err: crate::error::AppResult<crate::workspace::RuntimeStatusDto> =
            Err(crate::error::AppError::Message("boom".into()));
        match classify_mcp_start(&err) {
            McpStartOutcome::Failed(reason) => assert!(reason.contains("boom")),
            McpStartOutcome::Running => panic!("Err 不得被当成成功"),
        }
    }

    #[test]
    fn only_listening_mcp_is_recorded_as_running() {
        // 记录「实际在跑」的集合：只有 Healthy（MCP 确实在监听）算运行。
        // 启动中 / MCP 错误 / 已停止都不能被记成运行，否则下次会误恢复。
        let listening = WorkspaceSample {
            id: "a".into(),
            name: "a".into(),
            mcp: ServiceSample::Healthy,
            tunnel_configured: false,
            tunnel_online: false,
        };
        assert!(state::mcp_listening(&listening));

        for mcp in [
            ServiceSample::Stopped,
            ServiceSample::Transitioning,
            ServiceSample::McpError,
        ] {
            let sample = WorkspaceSample {
                id: "a".into(),
                name: "a".into(),
                mcp,
                tunnel_configured: false,
                tunnel_online: false,
            };
            assert!(
                !state::mcp_listening(&sample),
                "{mcp:?} 不应被记为运行中"
            );
        }
    }

    #[test]
    fn multi_workspace_without_name_falls_back_safely() {
        // 多工作区但读不到名字时不能崩，也不应回退成别的名字。
        let text = autostart_label_text(2, &AutostartTarget::Bound("id-9".into()), None);
        assert_eq!(text, format!("{BASE}（未知工作区）"));
    }
}
