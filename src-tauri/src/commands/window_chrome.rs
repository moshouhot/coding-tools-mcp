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
