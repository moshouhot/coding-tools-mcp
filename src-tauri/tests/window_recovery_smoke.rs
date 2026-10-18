//! Opt-in native Wry regression; does not load AppState, user profiles or services.
//! Run: cargo test --features ui-smoke,tauri/custom-protocol --test window_recovery_smoke
#![allow(dead_code)]

#[path = "../src/error.rs"]
mod error;

mod platform {
    #[cfg(windows)]
    pub mod windows {
        pub mod process {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/platform/windows/process.rs"));
        }
    }
}

mod commands {
    pub mod ui_memory {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands/ui_memory.rs"));
    }
    pub mod window_chrome {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands/window_chrome.rs"));
    }
}

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

struct SmokeLogger;
impl log::Log for SmokeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }
    fn flush(&self) {}
}

async fn assert_shown(app: &AppHandle, scenario: &str) {
    let window = app.get_webview_window("main").expect("main must exist");
    assert!(window.is_visible().unwrap(), "{scenario}: visible");
    assert!(!window.is_minimized().unwrap(), "{scenario}: restored");
    #[cfg(windows)]
    assert!(!window.hwnd().unwrap().0.is_null(), "{scenario}: native HWND");
    println!("PASS {scenario}: label=main visible=true minimized=false");
}

async fn exercise(app: &AppHandle) -> error::AppResult<()> {
    use commands::window_chrome::{hide_to_tray, request_show_main_window, show_main_window};
    assert!(app.get_webview_window("main").is_none());
    println!("REPRO main absent; configured window exists");
    if std::env::args().any(|argument| argument == "--expect-unavailable") {
        let error = show_main_window(app.clone()).await.expect_err("broken Runtime must fail");
        assert!(error.to_string().contains("主窗口未成功创建"), "{error}");
        println!("PASS unavailable Runtime is reported: {error}");
        return Ok(());
    }
    show_main_window(app.clone()).await?;
    assert_shown(app, "recover missing window").await;

    hide_to_tray(app.clone())?;
    show_main_window(app.clone()).await?;
    assert_shown(app, "show hidden window").await;
    app.get_webview_window("main").unwrap().minimize().unwrap();
    show_main_window(app.clone()).await?;
    assert_shown(app, "restore minimized window").await;

    hide_to_tray(app.clone())?;
    request_show_main_window(app.clone());
    for _ in 0..100 {
        if app.get_webview_window("main").unwrap().is_visible().unwrap() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_shown(app, "tray/single-instance dispatch").await;

    app.get_webview_window("main").unwrap().destroy().unwrap();
    for _ in 0..100 {
        if app.get_webview_window("main").is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (first, second) = tokio::join!(show_main_window(app.clone()), show_main_window(app.clone()));
    first?;
    second?;
    assert_eq!(app.webview_windows().len(), 2, "one main plus isolated keepalive");
    assert_shown(app, "concurrent recovery").await;

    let (recreate, show) = tokio::join!(
        commands::ui_memory::recreate_ui_webview(app.clone()),
        show_main_window(app.clone())
    );
    recreate?;
    show?;
    assert_shown(app, "show during WebView replacement").await;
    assert_eq!(app.webview_windows().len(), 2, "recreate keepalive must be removed");
    // Leave a short interval for independent UI Automation / HWND inspection.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    Ok(())
}

fn main() {
    log::set_logger(&SmokeLogger).expect("smoke logger");
    log::set_max_level(log::LevelFilter::Warn);
    let outcome = Arc::new(AtomicI32::new(2));
    let setup_outcome = outcome.clone();
    let quitting = Arc::new(AtomicBool::new(false));
    let setup_quitting = quitting.clone();
    let mut context = tauri::generate_context!();
    context.config_mut().identifier.push_str(".window-recovery-smoke");
    context.config_mut().app.windows[0].create = false;
    context.config_mut().app.windows[0].title = "Coding Tools MCP - isolated recovery test".into();
    // Keep the native event loop alive while reproducing the missing main UI.
    let mut keepalive = context.config().app.windows[0].clone();
    keepalive.label = "__test_keepalive__".into();
    keepalive.create = true;
    keepalive.visible = false;
    keepalive.skip_taskbar = true;
    context.config_mut().app.windows.push(keepalive);
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let handle = app.handle().clone();
            let watchdog = handle.clone();
            let watchdog_quitting = setup_quitting.clone();
            let watchdog_outcome = setup_outcome.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                eprintln!("FAIL native window smoke timed out");
                watchdog_outcome.store(2, Ordering::SeqCst);
                watchdog_quitting.store(true, Ordering::SeqCst);
                watchdog.exit(2);
            });
            tauri::async_runtime::spawn(async move {
                let test_handle = handle.clone();
                let result = tauri::async_runtime::spawn(async move { exercise(&test_handle).await }).await;
                let code = match result {
                    Ok(Ok(())) => 0,
                    Ok(Err(error)) => {
                        eprintln!("FAIL {error}");
                        1
                    }
                    Err(error) => {
                        eprintln!("FAIL smoke task: {error}");
                        1
                    }
                };
                setup_outcome.store(code, Ordering::SeqCst);
                setup_quitting.store(true, Ordering::SeqCst);
                handle.exit(code);
            });
            Ok(())
        })
        .build(context)
        .expect("native smoke app should build");
    let code = app.run_return(move |_, event| {
        match &event {
            tauri::RunEvent::Ready => println!("EVENT Ready"),
            tauri::RunEvent::ExitRequested { code, .. } => println!("EVENT ExitRequested {code:?}"),
            tauri::RunEvent::WindowEvent { label, event, .. } => println!("EVENT Window {label} {event:?}"),
            tauri::RunEvent::Exit => println!("EVENT Exit"),
            _ => {}
        }
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if !quitting.load(Ordering::SeqCst) {
                api.prevent_exit();
            }
        }
    });
    println!("EVENT run_return={code}");
    std::process::exit(outcome.load(Ordering::SeqCst));
}
