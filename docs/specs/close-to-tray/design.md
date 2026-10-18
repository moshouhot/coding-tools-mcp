# 设计文档：close-to-tray

## 概述

对应需求 FR-1～FR-5：关窗时**直接隐藏到托盘**（不再弹确认框），保持 MCP/Actions/隧道继续运行；托盘「退出」真退出；与 `recreate_ui_webview` 隐藏态协同；Windows 二次启动尽量唤起已有窗口。

> **行为变更（v0.2.4-custom.7）**：原设计为「应用内三按钮确认（取消 / 后台运行 / 直接关闭）」。
> 实测下来每次关窗都要做一次选择成本过高，用户明确要求取消弹窗、直接最小化。
> 真退出改由托盘菜单「退出」承担，语义更清晰：**窗口关闭 ≠ 退出程序**。

## 技术方案

### 关闭拦截（Rust 单点）

关窗只在 Rust 侧处理，前端不再挂钩：

1. `on_window_event(CloseRequested)` 中，若 `should_intercept_close()` 为真
   → 调 `hide_to_tray()` 隐藏主窗，并 `api.prevent_close()`。
2. **隐藏失败时不阻止关闭**：否则会出现「已拦截 + 没藏成功」的不一致状态，
   用户还可能误以为程序消失。失败走 stderr 记录，窗口保持原状。
3. `ALLOW_EXIT`（托盘「退出」/ `quit_app`）为真时不拦截，允许真正退出。
4. `UI_RECREATING` 为真时也不拦截，保证 `recreate_ui_webview` 的 `destroy` 不被拦住。

**为什么只保留 Rust 一处：** 原实现前端 `onCloseRequested` 也调了
`event.preventDefault()` 并弹对话框。两处并存时，只改掉 Rust 侧仍然会弹窗，
因此前端 hook 与对话框一并移除，避免留下"看起来没生效"的第二个拦截点。

**为什么用 `hide` 而不是 `minimize`：** Windows 最小化会留下
`(-32000, -32000)` 几何哨兵，UI 重建时容易把窗口丢到屏幕外
（0.1.32/33 修过的坑）。托盘语义本就该是隐藏，不是任务栏最小化。

### 托盘（Rust）

`tauri::tray::TrayIconBuilder` 创建状态图标，菜单含状态行、「显示窗口」、「开机自启」、「退出」。
左键点击显示并聚焦主窗。托盘创建失败会使应用启动失败（`setup_tray(app)?`），
因此走到关窗逻辑时托盘必然可用，`hide` 是安全操作。

### UI 重建协同

1. 重建前记录 `was_hidden` / `was_minimized` / `is_maximized`。
2. 重建用 keepalive 窗口保证「销毁最后一个窗口不会退出进程」。
3. 重建后：若 `was_hidden`，则 `hide()` 且不要 `set_focus`；否则保持现有 show/focus/minimize 逻辑。
4. CloseRequested 兜底在 `UI_RECREATING==true` 时不 `prevent_close`（允许 destroy）。

### 二次启动（Windows）

`acquire_single_instance` 发现已存在实例时向已有进程发唤起信号，
主实例后台线程 wait 后 `show_main_window`。唤起失败至少 eprintln，不静默 return。

### 退出路径

真退出只有两处入口，均先 `arm_allow_exit()` 再 `app.exit(0)`：

- 托盘菜单「退出」
- `quit_app` 命令

`hide_to_tray` 与 `quit_app` 命令仍保留注册（供托盘与外部调用），
但前端已无 `quitApp` / `hideToTray` 的消费者。

## 关键决策

1. **关窗默认隐藏，不弹确认**：减少每次关窗的决策成本；真退出交给托盘菜单。
2. **拦截只保留 Rust 一处**：两处并存时改一处不生效，是最容易踩的坑。
3. **隐藏失败不阻止关闭**：避免"已拦截但没藏成功"的僵尸状态。
4. **托盘隐藏用 `hide` 而非 `minimize`**：规避最小化几何哨兵问题。
5. **真退出设允许标志再关窗**：避免 CloseRequested 再次拦截。

## 文件结构

| 文件 | 变更 |
|------|------|
| `src-tauri/src/lib.rs` | CloseRequested → `hide_to_tray`；移除前端 emit |
| `src-tauri/src/commands/window_chrome.rs` | `should_intercept_close` / `hide_to_tray` / `arm_allow_exit` + 单测 |
| `src-tauri/src/commands/ui_memory.rs` | `was_hidden` 恢复 |
| `src/routes/+layout.svelte` | 移除关闭确认对话框与 close-guard 接线 |
| `src/app.css` | 移除 `.tx-close-*` 样式 |
| ~~`src/lib/components/CloseConfirmDialog.svelte`~~ | **已删除** |
| ~~`src/lib/close-guard.ts`~~ | **已删除**（前端不再拦截关窗） |
