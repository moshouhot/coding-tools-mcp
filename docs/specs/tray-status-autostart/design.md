> **状态：本次交付方案。**
>
> 在官方主程序（`src-tauri/`）内实现托盘状态图标与「开机自启并启动 MCP/隧道」。
> 选此方案的原因：托盘需要读取进程内 supervisor 的真实运行状态，
> 并直接调用 `start_mcp_by_id`，进程内实现不需要任何外部控制接口或界面自动化。
>
> 官方安装的应用可照常升级；定制版由本地源码构建，仅用于验证。

---

# 设计文档：tray-status-autostart

## 架构与模块划分

新增 `src-tauri/src/tray/` 模块，把原 `lib.rs::setup_tray` 的托盘职责收拢进来：

| 文件 | 职责 |
| --- | --- |
| `tray/mod.rs` | 托盘构建、菜单事件、5 秒轮询、状态应用、自启开关与自启启动路径 |
| `tray/icon.rs` | 两态图标：运行中用原图标原样，未运行用同一图标的灰度版 |
| `tray/autostart.rs` | HKCU Run 读写、命令构造、`--autostart` 参数识别、可替换后端（便于测试） |
| `tray/state.rs` | 采样模型与聚合：单工作区 → 两态总状态 + tooltip + summary |

`lib.rs` 只保留 `setup_tray` 薄封装，并在 `setup` 中启动轮询、按 `--autostart` 触发自启启动。

## 状态模型

```text
WorkspaceSample { name, mcp, tunnel_configured, tunnel_online }
        │
        ├─ classify(): Healthy + 配置了隧道但未连 → TunnelOffline
        │               McpError 保持独立，不归为「运行中」
        ▼
aggregate(Vec<WorkspaceSample>) → TrayState { level, tooltip, summary, any_running, workspaces }
```

档位判定（图标只有两态）：

1. 存在 `Healthy`（MCP 确实在监听；隧道健康与否不影响图标） → **运行中**（原图标）
2. 否则 → **未运行**（灰度）

`Transitioning`（启动/停止中）、`TunnelOffline`（隧道未连）、`McpError`（MCP 启动失败）
**都不改变图标**，只体现在 summary 与 tooltip 文字里。

**MCP 错误 ≠ 隧道离线**：`McpError` 不计入运行数，summary 显示「MCP 启动失败」，
不会错报成「运行中 · 隧道未连接」。

采样来源：

- **MCP 阶段**：`RuntimeSupervisor::refresh_mcp()` + `mcp_status()`。`refresh` 会实际校验监听端口是否存活，避免只信内存 phase。
- **隧道状态**：`TunnelSupervisor::status(profile, Mcp, &settings).state == "running"`，该实现会检查 frpc 进程存活与 session 是否存在。**不读取保存过的公网 URL**。

> 范围声明：运行中图标只表示**本机** MCP 监听已建立，
> 不代表隧道公网端到端可达（这需要外部探测，本次不做）。

## 图标生成

图标只有两态，且**不做重新着色**：

1. **运行中**：直接复用 Tauri 的默认窗口图标（`app.default_window_icon()`），
   与原作者的托盘图标完全一致。
2. **未运行**：取同一张源图，按 Rec.601 亮度加权转灰度（`alpha` 原样保留），
   只计算一次并缓存。
3. 若拿不到默认窗口图标，回退到编译期 `include_bytes!` 内嵌的 `icons/128x128.png`。

因为只改颜色、不改形状，两种状态的轮廓完全一致，用户一眼能看出「同一个图标的亮/暗」。

托盘图标通过 `TrayIcon::set_icon(Some(...))` 原地替换，不重建托盘。

## 自启设计

**命令**：`"<exe 绝对路径>" --autostart`（引号包裹，兼容含空格/中文路径）。
**注册表**：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，值名 `CodingToolsMcpDesktop`。
**三态判定**（`RunState`）：

| 状态 | 含义 |
| --- | --- |
| `Enabled` | 值存在且与本 exe 预期命令一致 |
| `Disabled` | 确认未启用（值不存在，或存在但指向其它副本） |
| `Unknown` | **读取失败**（权限/损坏） |

> `Unknown` 绝不等于 `Disabled`。若把读取失败当成「未启用」，一次权限错误就会导致
> 关闭流程错误地清空绑定。因此 `read_run_value()` 返回 `Result<Option<String>, String>`，
> 只有「值不存在」是 `Ok(None)`。

**可注入后端**：`AutostartOps` trait 抽象注册表与绑定的读写，生产用 `LiveOps`
（真实注册表 + 数据存储），测试用内存 `FakeOps` 可注入写入失败、读回不生效、
读取失败，从而验证真实回滚行为而非仅返回值。

**开启流程**（`autostart::enable`，由 `on_toggle_autostart` 调用）：

1. 读取当前 `RunState`。`Unknown`（读取失败）→ 直接取消操作，**不冒险修改**。
2. `selected_workspace_id()` 取用户**明确选中**的工作区。**不回退到首个工作区**；
   未选中 → 提示「请先在界面中打开一个工作区」，不写注册表。
3. 记录旧注册表值与旧绑定 → 写注册表 → 保存绑定 → 读回校验。
4. **任一步失败都回滚**到旧状态（恢复旧 Run 值与旧绑定）；回滚本身失败也会
   在错误信息里明确报告，不静默。
5. 只有写入、绑定、读回全部成功才算成功。

**关闭流程**（`autostart::disable`）：

1. 删除注册表项。
2. **读回确认已不存在**（`Ok(None)`）；仍存在或读取失败 → 报错并**保留绑定**。
3. 确认后才清除绑定。

全程**不停止**任何正在运行的服务，也**不触碰其它应用**的 Run 项。

**启动流程**（`--autostart`）：`setup` 中隐藏主窗口 → 后台 `run_autostart_startup()`：

1. `autostart_launch_target()` 解析绑定目标。**此路径绝不回退**：
   - `Unbound` → 提示未绑定并跳过；
   - `Missing` → 提示绑定工作区已不存在并跳过，**明确不启动其它工作区**；
   - `Bound` → 继续。
2. `start_mcp_by_id()` 启动 MCP（复用既有隧道联动）。
3. **回查隧道实际监督状态**再决定文案：
   - 隧道在线 → 「已启动 MCP 与隧道」；
   - 已配置但未在线 → 「MCP 已启动，但隧道未能连接」；
   - 未配置隧道 → 「MCP 已启动（未配置隧道）」。
   （`start_mcp_service` 会吞掉隧道启动错误，所以不能凭 MCP `running` 就宣布隧道成功。）

## 提示可见性

`tooltip` 会被下一轮 5 秒刷新覆盖，release 下 stderr 也不可见。因此：

- **失败**：弹一个系统错误对话框（`tauri-plugin-dialog`，`MessageDialogKind::Error`），
  用户必定能看到；同时写 stderr 日志。
- **成功**：只记日志。成功无需打断用户——菜单勾选、状态行与图标已反映结果。

> 早期版本曾在菜单里加一行只读「提示」行，现已移除：它常驻在菜单里、
> 且成功提示也会留下痕迹，反而干扰用户。

## 关键决策与取舍

| 决策 | 原因 |
| --- | --- |
| 用 `windows` crate 的 `Win32_System_Registry`，不引入 `winreg` | 项目已依赖 `windows`，只需加一个 feature，避免新增第三方依赖 |
| 状态轮询 5 秒、串行、`AtomicBool` 防重入 | 与参考实现一致；避免重叠采样 |
| 图标仅在**档位变化**时 `set_icon`，tooltip/summary 仅在内容变化时更新 | 避免每 5 秒无谓重绘与托盘闪烁 |
| 图标只做两态，不做重新着色 | 运行中保持原作者图标样式，用户一眼可辨；不擅自改动品牌配色 |
| 失败用系统对话框，而非菜单里的提示行 | 失败必须真的被看到；提示行常驻菜单反而干扰 |
| 开启与启动用**两个不同**的目标解析函数 | 开启只用用户明确选中的工作区；登录启动严格用绑定项，否则会静默换目标 |
| 非 Windows 不展示自启菜单项 | 不给用户一个必然失败的操作 |
| 自启只启动 MCP，不启动 Actions | 用户诉求明确为 MCP/隧道；减少登录时的资源占用与端口冲突面 |
| 绑定工作区 id 持久化在 `profiles.json` | 与既有配置同源，避免新增配置文件 |

## 兼容性与风险

- `autostart_workspace_id` 使用 `#[serde(default)]`，旧 `profiles.json` 可直接反序列化，无需迁移。
- 非 Windows 平台：`autostart` 的 win 后端为桩实现（读 `None`、写 `false`），菜单项仍存在但不会真正生效；不影响 macOS 既有行为。
- 端口被占用等启动失败由 `start_mcp_by_id` 既有错误路径返回，并通过托盘提示可见，不静默改启动其它工作区。
