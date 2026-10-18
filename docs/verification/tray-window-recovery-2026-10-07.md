# 托盘显示窗口无响应：环境与代码修复

## 现场与根因

用户指定安装版：`C:\Users\Administrator.DESKTOP-4A6KNOF\AppData\Local\Coding Tools MCP\coding-tools-mcp-desktop.exe`。
原进程 PID 35372 有托盘和 Tao 事件窗口，没有主窗口 HWND，也没有 WebView2 子进程；后台服务仍监听 28766。

项目所依赖的官方 WebView2Loader 默认查询返回 `103.0.1264.77`。该版本目录仅剩启动 EXE、签名和 EmbeddedBrowserWebView.dll，缺少 ICU 等运行文件；默认初始化打印 `Invalid file descriptor to ICU data received`，Tauri 随后无法创建主窗口。

系统安装登记 `Clients\pv` 是 `154.0.4258.53`，但以下实际加载指针仍是旧目录：

```text
HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\ClientState\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}
EBWebView = C:\Program Files (x86)\Microsoft\EdgeWebView\Application\103.0.1264.77
```

进程级指定完整的 `154.0.4258.53` 后，Loader 返回正确版本，原生窗口恢复测试成功。指定路径必须使用 Windows 反斜杠；初次正斜杠实验返回 0x80070002，不能据此判断新版 Runtime 损坏。

原代码另有两个缺口：主窗口对象丢失时不重建；托盘和单实例唤醒忽略错误。Tauri/Wry 的异步创建还可能先返回对象，再在事件循环里创建失败，因此 `build()`、`show()` 返回成功不能作为窗口实际出现的证据。

## 代码修改

- `window_chrome.rs`：异步恢复，缺失时按配置重建，显示后查询可见性，失败返回错误并由托盘/单实例入口提示。
- `ui_memory.rs`：与恢复共用生命周期锁；重建窗口实际响应后才移除 keepalive。
- `tray/mod.rs`、`lib.rs`：统一通过异步请求入口处理显示操作。
- `window_recovery_smoke.rs`：隔离原生测试，不加载用户配置、AppState 或后台服务，失败必须非零退出；测试 EXE 嵌入 Common Controls v6 清单。

## 验证及边界

回归覆盖缺失窗口、隐藏、最小化、托盘/单实例共用分发入口、并发恢复和内存刷新并发显示；查询真实 HWND、可见性与最小化状态。另用残缺 Runtime 验证失败不会被误报成功。测试不等于已安装 EXE 的真实托盘菜单验收。

前端类型检查与前端构建通过。最终 release 原生测试的六个正常恢复场景全部通过；同一测试 EXE 切换到残缺 103 后，也通过“明确返回创建失败”的负面回归。日志分别为 `tmp-run/tray-window/native-smoke-final-positive.log` 和 `tmp-run/tray-window/native-smoke-final-negative-direct.log`。不把 Cargo 构建返回值代替运行验收。正式 release 构建通过，日志为 `tmp-run/tray-window/release-build-final.log`。

交付目录为 `package/tray-window-fix-20261007/`，包括修复 EXE、已安装旧 EXE 的准确备份 `installed-before.exe` 以及记录二进制与源码 SHA256 的 `manifest.json`。原生测试与正式生产 EXE 使用同一组修改过的窗口模块。没有提交或发布。

Oracle Browser 初审、遇阻审查和最终审查均通过本地 MCP set/get 绑定后只读读取证据。最终请求 `tray-window-final-20261007-03` 返回 **PASS（代码与现有验证证据通过；不代表部署完成）**，审查记录为 `tmp-run/tray-window/oracle-final-review.log`。

## 已批准并完成的本机应用

1. 备份上述注册表 **单个 EBWebView 值**，将其改为 `C:\Program Files (x86)\Microsoft\EdgeWebView\Application\154.0.4258.53`。不清理旧目录，不调整其他注册表值。
2. 停止旧桌面进程，核对已备份的安装 EXE，再替换为本次构建的修复版并重启。工作区、密钥、服务配置保持原内容；重启期间本机 MCP 和隧道会短暂中断。
3. 用默认 Loader 查询版本并重新执行原生回归，再核对安装 EXE 路径、主 HWND、WebView2 子进程与后台监听。真实托盘点击及页面可交互验收仍须完成。

用户已在本对话明确批准两项并重启。实际应用结果：

- 单个 `EBWebView` 值已从 103 目录改指向 154，原值备份：`tmp-run/tray-window/backup/webview2-EBWebView-20261007-133458-339.json`。默认 Loader 版本查询返回 `154.0.4258.53`。
- 安装 EXE 已替换并与交付包 SHA256 一致：`27E2FBE33C0E50A69077E62BCF1759393B2F381C7FFE48CAF41269D79A739AF1`。旧 EXE 保存在交付目录的 `installed-before.exe`。
- 原 PID 35372 已停止，新 PID 41464 从同一安装路径运行。仅清理了已验证属于旧实例的 cloudflared 子进程，新实例已恢复 tunnel。
- 独立 Win32 回读：主 HWND 10239272，标题 Coding Tools MCP，visible=true，minimized=false，enabled=true，窗口矩形 `[26,26,1322,865]`，存在 WRY_WEBVIEW 和 Chrome renderer 子窗口。记录：`tmp-run/tray-window/installed-native-readback.json`。
- 新实例的 WebView2 子进程来自完整 154 Runtime，127.0.0.1:28766 监听属于 PID 41464；真实 MCP 连接器 `get_active_project` 已鉴权返回 `ok=true`。
- 不指定 Runtime 的同一原生测试 EXE 再次运行，六个恢复场景全部通过，退出码 0。记录：`tmp-run/tray-window/native-smoke-default-after-repair.log`。

修复操作没有直接编辑 profiles.json 或凭据。应用启动后自行写回 profiles.json，文件 SHA256 发生变化；启动前只记录了文件哈希，没有保存可做完整逐字段对照的 JSON 镜像，因此不声称该文件字节完全不变。新实例使用现有 MCP 连接器成功鉴权。历史备份不能代替此次启动前镜像，也没有被回写。

已请求用户验证实际托盘“显示窗口”和页面切换，等待反馈；native 回读与测试不能代替这项交互验收。尚未提交或推送。

## 用户追加：清理残缺 103 Runtime

用户明确要求删除 `103.0.1264.77`。已核对精确目录及全部子项没有重解析点，没有进程从该目录运行；三个文件已压缩到交付目录 `legacy-webview2-103.0.1264.77.zip`，每个 ZIP 条目的 SHA256 与原文件匹配，哈希清单为同目录 `legacy-webview2-103.0.1264.77.manifest.json`。

递归删除被 Accidental Delete Guard 拒绝：`ADG_BLOCK: 递归删除目标逃出当前项目/Workspace 允许范围`。103 目录尚未删除，没有关闭保护或换删除方式绕过。已生成 `legacy-removal-approval.md` 和 `legacy-removal-guard-proposed.json` 供审批：需要暂时扩展允许范围、保护所有现存兄弟目录，删除后立即恢复原配置。这项全局保护配置变更尚未获批、尚未执行。
