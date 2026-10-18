use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

use crate::error::{AppError, AppResult};

/// When true, CloseRequested may destroy the window / exit the process.
static ALLOW_EXIT: AtomicBool = AtomicBool::new(false);

/// 收到 `CloseRequested` 时该做什么。
///
/// 抽成纯函数是为了让「拦截就必须阻止关闭」这条不变量**可测**。
/// 它被现场 bug 破坏过：旧实现只有隐藏成功才 `prevent_close`，
/// 失败时放行 → 窗口被销毁 → 进程退出 → 服务全断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// 用户已请求退出，或 WebView 正在重建：放行，允许销毁窗口。
    AllowClose,
    /// 拦截：**先**阻止关闭，**再**隐藏到托盘。
    ///
    /// 两步的先后与配对关系是硬约束，不能拆开判断。
    PreventCloseThenHide,
}

impl CloseOutcome {
    pub fn prevents_close(self) -> bool {
        matches!(self, CloseOutcome::PreventCloseThenHide)
    }
}

/// 纯决策：只取决于「是否已请求退出」与「是否正在重建 UI」。
///
/// 注意它**不**接收"托盘是否可用"或"隐藏是否成功"—— 那些都不该影响
/// 是否阻止关闭。隐藏失败的正确表现是"窗口留在原地"，不是"放行关闭"。
pub fn close_outcome(allow_exit: bool, ui_recreating: bool) -> CloseOutcome {
    if allow_exit || ui_recreating {
        CloseOutcome::AllowClose
    } else {
        CloseOutcome::PreventCloseThenHide
    }
}

/// 由当前全局标志推导关窗动作。
pub fn current_close_outcome() -> CloseOutcome {
    close_outcome(
        ALLOW_EXIT.load(Ordering::SeqCst),
        crate::commands::ui_memory::should_prevent_exit(),
    )
}

fn main_window(app: &AppHandle) -> AppResult<WebviewWindow> {
    app.get_webview_window("main")
        .or_else(|| {
            app.webview_windows()
                .into_iter()
                .find(|(label, _)| !label.starts_with("__"))
                .map(|(_, w)| w)
        })
        .ok_or_else(|| AppError::Message("找不到主窗口".into()))
}

#[tauri::command]
pub fn hide_to_tray(app: AppHandle) -> AppResult<()> {
    let window = main_window(&app)?;
    window
        .hide()
        .map_err(|err| AppError::Message(format!("隐藏窗口失败: {err}")))?;
    Ok(())
}

#[tauri::command]
pub async fn show_main_window(app: AppHandle) -> AppResult<()> {
    // WebView creation must run outside the Windows event-loop callback. Share
    // the gate with memory recreation so concurrent requests create only one UI.
    let _guard = crate::commands::ui_memory::UI_WINDOW_GATE.lock().await;
    let window = match main_window(&app) {
        Ok(window) => window,
        Err(_) => {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|config| config.label == "main")
                .ok_or_else(|| AppError::Message("找不到主窗口配置".into()))?;
            tauri::WebviewWindowBuilder::from_config(&app, config)
                .and_then(|builder| builder.build())
                .map_err(|err| AppError::Message(format!("重建主窗口失败: {err}")))?
        }
    };
    window
        .unminimize()
        .map_err(|err| AppError::Message(format!("恢复窗口失败: {err}")))?;
    window
        .show()
        .map_err(|err| AppError::Message(format!("显示窗口失败: {err}")))?;
    window
        .set_focus()
        .map_err(|err| AppError::Message(format!("激活窗口失败: {err}")))?;
    // On Windows build/show can only enqueue creation. A getter waits for the
    // event loop and detects a failed WebView creation instead of reporting Ok.
    let visible = window.is_visible().map_err(|err| {
        AppError::Message(format!(
            "主窗口未成功创建，请检查 WebView2 Runtime 并重新启动程序: {err}"
        ))
    })?;
    if !visible {
        return Err(AppError::Message("主窗口仍处于隐藏状态".into()));
    }
    Ok(())
}

/// Tray callbacks and the single-instance waiter cannot await an IPC command.
pub fn request_show_main_window(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = show_main_window(app.clone()).await {
            eprintln!("[window] {error}");
            app.dialog()
                .message(error.to_string())
                .title("显示窗口失败")
                .kind(MessageDialogKind::Error)
                .show(|_| {});
        }
    });
}

#[tauri::command]
pub fn quit_app(app: AppHandle) -> AppResult<()> {
    arm_allow_exit();
    app.exit(0);
    Ok(())
}

pub fn arm_allow_exit() {
    ALLOW_EXIT.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拦截关窗时**必须**阻止关闭。
    ///
    /// 回归锁定现场 bug：旧实现只在隐藏成功时 prevent_close，
    /// 隐藏失败放行 → 窗口销毁 → 最后一个窗口消失 → 进程退出 → 服务全断。
    #[test]
    fn interception_always_prevents_close() {
        let outcome = close_outcome(false, false);
        assert_eq!(outcome, CloseOutcome::PreventCloseThenHide);
        assert!(
            outcome.prevents_close(),
            "拦截关窗必须阻止关闭，且与隐藏成败无关"
        );
    }

    /// 只有「已请求退出」或「正在重建 UI」才允许销毁窗口。
    #[test]
    fn only_exit_or_recreate_allows_close() {
        assert_eq!(close_outcome(true, false), CloseOutcome::AllowClose);
        assert_eq!(close_outcome(false, true), CloseOutcome::AllowClose);
        assert!(!CloseOutcome::AllowClose.prevents_close());
    }

    /// 重建优先于退出标志：既要能销毁窗口，也不会误拦重建。
    #[test]
    fn recreate_takes_precedence_and_allows_destroy() {
        assert_eq!(close_outcome(true, true), CloseOutcome::AllowClose);
    }

    /// 穷举所有输入组合，确认没有"拦截但放行"的漏洞。
    #[test]
    fn no_combination_intercepts_without_preventing_close() {
        for allow_exit in [false, true] {
            for recreating in [false, true] {
                let outcome = close_outcome(allow_exit, recreating);
                let should_intercept = !(allow_exit || recreating);
                assert_eq!(
                    outcome.prevents_close(),
                    should_intercept,
                    "allow_exit={allow_exit} recreating={recreating} 行为不一致"
                );
            }
        }
    }
}
