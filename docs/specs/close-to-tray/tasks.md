# 任务清单：close-to-tray

## 概述

实现关窗**直接最小化到托盘**（不再弹确认框）、托盘恢复、真退出，并与 UI 重建 / Windows 单实例协同。对应 GitHub #8。

> **行为变更（v0.2.4-custom.7）**：原方案关窗弹三按钮确认，已改为直接隐藏。
> 下文历史任务项保留原貌以留存演进过程，当前有效行为以 `design.md` 为准。

> **二元禁令（零容忍）**：禁止出现未替换占位符、`TODO`、省略实现。

---

## 交付物清单（Scope-lock）

- **预计新建文件数**: 4 个
- **预计修改文件数**: 7 个
- **交付物逐项列举**:
  1. `src-tauri/src/commands/window_chrome.rs`（新建）
  2. `src/lib/components/CloseConfirmDialog.svelte`（新建）
  3. `src/lib/close-guard.ts`（新建）
  4. `src/lib/api/window-chrome.ts`（新建）
  5. `src-tauri/Cargo.toml`（修改）
  6. `src-tauri/src/lib.rs`（修改）
  7. `src-tauri/src/commands/mod.rs`（修改）
  8. `src-tauri/src/commands/ui_memory.rs`（修改）
  9. `src-tauri/capabilities/default.json`（修改，若需要）
  10. `src/routes/+layout.svelte`（修改）
  11. `src/app.css`（修改）

---

## 任务列表

### 阶段 1: Rust 托盘与窗口命令

- [x] 1.1 `Cargo.toml` 启用 `tray-icon`；`window_chrome` 模块：`ALLOW_EXIT`、`hide_to_tray`、`show_main_window`、`quit_app`、`should_allow_window_close`
  - **证据块**: 现状 `lib.rs` 仅有 `ExitRequested` + `should_prevent_exit`；`Cargo.toml` 中 `tauri` features 为空。
  - **涉及文件**: `Cargo.toml`、`commands/window_chrome.rs`、`commands/mod.rs`
  - _需求: FR-2, FR-3_ ｜ _设计: 托盘 / 命令_

- [x] 1.2 `lib.rs` setup 创建托盘菜单与点击；`CloseRequested` 在不允许退出且非 UI 重建时 `prevent_close`；注册 commands
  - **证据块**: `lib.rs` builder 链与 `run` 事件循环；capabilities 已有 `core:default`。
  - **涉及文件**: `src-tauri/src/lib.rs`
  - _需求: FR-1, FR-2, FR-3_ ｜ _设计: 关闭拦截 / 托盘_

- [x] 1.3 `recreate_ui_webview` 记录 `was_hidden`，重建后隐藏态保持 hide
  - **证据块**: `ui_memory.rs` 已处理 `was_minimized`；需对称增加 `was_hidden`。
  - **涉及文件**: `src-tauri/src/commands/ui_memory.rs`
  - _需求: FR-4_ ｜ _设计: UI 重建协同_

- [x] 1.4 Windows 二次启动：已存在实例时触发命名事件唤起主窗
  - **证据块**: `lib.rs` `acquire_single_instance` 在 `ERROR_ALREADY_EXISTS` 时直接 return。
  - **涉及文件**: `src-tauri/src/lib.rs` 或 `platform/windows`
  - _需求: FR-5_ ｜ _设计: 二次启动_

### 阶段 2: 前端确认框

- [x] 2.1 `CloseConfirmDialog` + `close-guard` + `api/window-chrome`；layout 挂载
  - **证据块**: `+layout.svelte` 已 `startUiMemoryGuard`；`app.css` 已有 `tx-btn-*`。
  - **涉及文件**: 新建 3 个前端文件、`+layout.svelte`、`app.css`
  - _需求: FR-1, FR-2, FR-3, NFR-3_ ｜ _设计: 对话框 UI_

### 阶段 3: 验证

- [x] 3.1 `cargo test` 相关 / 编译通过；`npm run check`；手动核对关窗三路径（开发机）
  - **证据块**: 记录命令退出码与手动路径结果。
  - _需求: 全部 FR_

---

## 实施顺序

1.1 → 1.2 → 1.3 → 2.1 → 1.4 → 3.1

---

## 需求覆盖矩阵

| 需求 | 任务 |
|------|------|
| FR-1 | 1.2, 2.1 |
| FR-2 | 1.1, 1.2, 2.1 |
| FR-3 | 1.1, 1.2, 2.1 |
| FR-4 | 1.3 |
| FR-5 | 1.4 |
| NFR-1～3 | 1.1, 1.2, 2.1, 3.1 |

## 文件变更清单

| 路径 | 操作 |
|------|------|
| `src-tauri/src/commands/window_chrome.rs` | 新建 |
| `src/lib/components/CloseConfirmDialog.svelte` | 新建 |
| `src/lib/close-guard.ts` | 新建 |
| `src/lib/api/window-chrome.ts` | 新建 |
| `src-tauri/Cargo.toml` | 修改 |
| `src-tauri/src/lib.rs` | 修改 |
| `src-tauri/src/commands/mod.rs` | 修改 |
| `src-tauri/src/commands/ui_memory.rs` | 修改 |
| `src-tauri/capabilities/default.json` | 修改（若需要） |
| `src/routes/+layout.svelte` | 修改 |
| `src/app.css` | 修改 |
