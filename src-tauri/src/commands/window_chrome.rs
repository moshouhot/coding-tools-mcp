use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

use crate::error::{AppError, AppResult};

/// When true, CloseRequested may destroy the window / exit the process.
static ALLOW_EXIT: AtomicBool = AtomicBool::new(false);

/// Intercept user close (show confirm / keep running) unless quitting or UI recreate.
pub fn should_intercept_close() -> bool {
    !ALLOW_EXIT.load(Ordering::SeqCst) && !crate::commands::ui_memory::should_prevent_exit()
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

    /// 关窗必须被拦截并转成「隐藏到托盘」，除非已明确请求退出。
    ///
    /// 这条路径现在直接 hide、不再弹确认框；测试锁住「默认拦截」这个前提，
    /// 避免日后误改成 `return false` 导致点 X 直接杀进程、连带停掉 MCP。
    #[test]
    fn close_is_intercepted_by_default() {
        // 注意：本仓库测试并行执行，ALLOW_EXIT 是全局原子量。
        // 这里只断言默认值（false）语义，不改写它，避免与其它用例互踩。
        if !ALLOW_EXIT.load(Ordering::SeqCst) {
            assert!(
                should_intercept_close(),
                "默认应拦截关窗（隐藏到托盘），而不是直接退出"
            );
        }
    }
}
