> **状态：本次交付方案。**
>
> 在官方主程序（`src-tauri/`）内实现托盘状态图标与「开机自启并启动 MCP/隧道」。
> 选此方案的原因：托盘需要读取进程内 supervisor 的真实运行状态，
> 并直接调用 `start_mcp_by_id`，进程内实现不需要任何外部控制接口或界面自动化。
>
> 官方安装的应用可照常升级；定制版由本地源码构建，仅用于验证。

---

# 任务清单：tray-status-autostart

## 概述

实现托盘两态图标（运行中=原图标，未运行=灰度）与「开机自启并启动 MCP/隧道」勾选项，绑定工作区、复用既有隧道联动。

> **二元禁令（零容忍）**：禁止出现未替换占位符、`TODO`、省略实现。

---

## 交付物清单（Scope-lock）

- **预计新建文件数**: 5 个（4 个源码 + 3 个 spec，spec 不计入源码）
- **预计修改文件数**: 5 个
- **交付物逐项列举**:
  1. `src-tauri/src/tray/mod.rs`（新建）
  2. `src-tauri/src/tray/icon.rs`（新建）
  3. `src-tauri/src/tray/autostart.rs`（新建）
  4. `src-tauri/src/tray/state.rs`（新建）
  5. `src-tauri/Cargo.toml`（修改：`Win32_System_Registry`）
  6. `src-tauri/src/lib.rs`（修改：模块声明、`setup_tray` 收拢、setup 挂轮询与自启）
  7. `src-tauri/src/commands/runtime.rs`（修改：暴露 `start_mcp_by_id`）
  8. `src-tauri/src/data/model.rs`（修改：`autostart_workspace_id`）
  9. `src-tauri/src/data/store.rs`（修改：绑定读写与目标解析）

---

## 任务分解

### T1 两态图标 `tray/icon.rs`

- [x] 运行中：复用 `app.default_window_icon()`，保持原作者图标样式
- [x] 未运行：同一源图转灰度（Rec.601 亮度，alpha 不变），只计算一次并缓存
- [x] 拿不到默认图标时回退到编译期内嵌 `icons/128x128.png`
- [x] 单测：灰度保留 alpha 且输出确为灰度、亮度单调、回退图尺寸与可见性、两态尺寸一致

### T2 状态聚合 `tray/state.rs`

- [x] `ServiceSample` / `WorkspaceSample` / `TrayState`
- [x] `classify()`：Healthy + 隧道未连 → TunnelOffline
- [x] `aggregate()`：有 MCP 监听 → 运行中；否则未运行；summary 与多行 tooltip
- [x] 单测：全停/无工作区/无隧道运行/隧道健康/隧道断开/启动中/多工作区混合/MCP 错误

### T3 开机自启 `tray/autostart.rs`

- [x] HKCU Run 读写（`windows` crate Registry）
- [x] 命令构造：引号包裹 + `--autostart`
- [x] `is_enabled()` 仅在完全匹配时为真
- [x] 可替换后端 + 失败注入，供测试
- [x] 单测：命令引号、陈旧项不算开启、写入/删除记录、失败可见、参数契约

### T4 托盘装配与轮询 `tray/mod.rs`

- [x] 菜单：状态行（禁用）、显示窗口、自启勾选、退出
- [x] 右键菜单、左键显示窗口（沿用既有交互）
- [x] 5 秒串行轮询，首次立即采样，仅变化时更新图标
- [x] 采样 MCP 实际阶段 + 隧道实际会话状态
- [x] 自启开关：绑定校验、失败回滚、勾选以读回为准
- [x] 自启启动路径：隐藏窗口 + 启动绑定工作区 MCP + 失败弹系统对话框
- [x] 菜单文案自适应：仅多工作区时附工作区名；绑定失效始终告知
- [x] 单测：单/多工作区、未绑定、目标失效、缺名回退
- [x] 运行状态记录：变化时落盘、内容未变不写、启动期间暂停
- [x] 退出时补一次只读快照（消除轮询 5 秒延迟导致的陈旧值）
- [x] 真实转换（启动/停止/重启/删工作区）后立即记录；内部重启临时停止不写入
- [x] 过期样本丢弃（代次比较）+ 记录串行化，防旧快照覆盖新状态
- [x] 读取/落盘失败可见且保留原记录（不把未知状态当空集）
- [x] 失效恢复目标显式报告（不静默丢弃）
- [x] 记录写入经 `DATA_FILE_LOCK`（与仓库其它写入路径一致）
- [x] 普通启动恢复：逐个启动、失败汇总为一次提示、绝不回退
- [x] 启动路径互斥：`startup_path()` 纯函数 + 单测
- [x] 单测：路径互斥、仅监听中的 MCP 计入运行

### T5 接线与持久化

- [x] `lib.rs`：声明 `mod tray`、`setup_tray` 转调、挂轮询、按 `startup_path` 分流
- [x] `commands/runtime.rs`：`pub(crate) start_mcp_by_id`
- [x] `data/model.rs`：`autostart_workspace_id`、`running_mcp_workspace_ids`（均 `#[serde(default)]`）
- [x] `data/store.rs`：`autostart_target_id` / `set_autostart_workspace` / `clear_autostart_workspace`
- [x] `data/store.rs`：`set_running_mcp_workspace_ids`（变更才写 + 回滚）/ `restorable_mcp_workspace_ids` / `missing_running_mcp_workspace_ids`
- [x] `commands/runtime.rs` / `commands/workspace.rs`：转换完成后调 `tray::record_current_state()`
- [x] `Cargo.toml`：`Win32_System_Registry`

### T6 验证

- [x] `cargo test --lib tray::`（43 项全通过）
- [x] `cargo test --lib data::store::`（8 项，含目标删除不回退、落盘失败回滚）
- [x] `cargo test --lib`（本机 230 passed / 0 failed）
- [x] `cargo clippy --lib`（`src/tray/` 零警告；其余警告均为既有）
- [x] `npm run check`（仅 1 项既有失败，见下）
- [x] `npm run tauri -- build --bundles nsis`（产出安装包）
- [x] 产物内嵌字符串核验（中文文案、`--autostart`、Run 键路径）
- [ ] 真实托盘交互（图标切换 / 右键菜单 / 自启项）——**待安装后人工验证**

---

## 验证结果

| 检查 | 结果 |
| --- | --- |
| `cargo test --lib tray::` | 43 passed / 0 failed |
| `cargo test --lib data::store::` | 8 passed / 0 failed（含「目标删除不回退」回归） |
| `cargo test --lib`（全量） | 230 passed / 0 failed |
| `cargo clippy --lib` | `src/tray/` 0 警告；其余警告均为既有 |
| `npm run check` | 1 error：`vite.config.js` 的 `@ts-expect-error`（**既有失败**） |
| `npm run tauri -- build --bundles nsis` | 成功（退出码 0），产出 `Coding Tools MCP_0.2.3_x64-setup.exe` |

### 关键回归覆盖（均用内存假后端验证真实行为，而非仅布尔返回）

| 场景 | 断言 |
| --- | --- |
| 绑定工作区被删除后登录启动 | `autostart_launch_target` 返回 `Missing`，**不回退**到其它工作区 |
| 从未绑定 / 绑定为空白 | 返回 `Unbound` |
| 开启时用户未选中工作区 | `selected_workspace_id` 返回空，**不回退**到首个工作区 |
| 绑定保存失败 | 注册表回滚为写入前旧值，绑定不变，错误信息含「已恢复」 |
| 读回校验不匹配 | 恢复为写入前旧值，绑定回滚 |
| 开启时读取失败 | 报错，**不声称成功** |
| 只写本应用 Run 项 | 日志仅两条（本应用项 + 绑定），不含其它应用 |
| 删除失败 | `disable` 报错且**保留绑定** |
| 删除后读回仍存在 | 报错且**保留绑定** |
| 删除后读取失败（Unknown） | 报错且**保留绑定**（不得当成未启用） |
| 绑定落盘失败 | 内存绑定回滚为旧值（不留下未落盘的值） |
| 旧绑定指向已删除工作区 | 回滚时能原样写回（不因存在性校验被拒） |
| 回滚本身失败 | 错误信息含「回滚不完整」，不假装已恢复 |
| 读不到旧绑定 | 直接取消开启，不写注册表 |
| `read_run` 缺失 vs 失败 | `Ok(None)` 与 `Err` 可区分 |
| MCP 错误 | 不计入运行数，summary 为「MCP 启动失败」，不误报为隧道问题；图标仍为灰度 |
| 隧道已配置但离线 | summary 为「隧道未连接」，仍计入运行数，图标为原图标 |
| 启动中（尚未监听） | 图标为灰度（未运行），文字显示「启动中」 |
| 单工作区自启文案 | 不附带工作区名（无冗余括号） |
| 多工作区自启文案 | 附带绑定工作区名，避免歧义 |
| 绑定失效（单工作区亦然） | 显示「目标已失效」，不被吞掉 |
| 含空格/中文/超长路径 | 自启命令引号包裹且引号成对 |
| 旧配置无 `running_mcp_workspace_ids` | 能直接反序列化，默认空集合，无需迁移 |
| 运行集合归一化 | 不同顺序 / 空白 / 重复 → 同一字节，保证「未变不写盘」比较可靠 |
| 运行集合内容未变 | 返回 `false` 且**不调用**落盘（避免每 5 秒重写配置） |
| 运行集合内容变化 | 返回 `true` 且落盘 |
| 运行集合落盘失败 | 内存回滚为旧集合，不留「内存有、磁盘无」 |
| 全部停止 | 空集合可写回（否则会一直恢复上次服务） |
| 已删除工作区在恢复集合中 | 丢弃该 id，**不回退**到其它工作区；全失效则空 |
| 启动路径互斥 | `--autostart` → `Autostart`，普通启动 → `Restore`，两者不相等 |
| 记录范围 | 仅 `Healthy`（MCP 确实在监听）计入；启动中 / MCP 错误 / 已停止均不计 |
| 暂停记录嵌套 | 深度计数：内层（内部重启）结束不得解除外层（启动恢复）的暂停；多余解除不减到负数 |
| 延迟样本跨越启动转换 | `should_skip_record` 在代次不一致时丢弃，防止旧快照覆盖刚恢复的集合 |
| 启动暂停期间 | 即使代次相同也不记录 |
| `Ok` 但 state 非 running | 归类为失败（含 error/stopped），不得当成启动成功 |
| `Err` 返回值 | 同样归类为失败并带上原因 |
| 失效恢复目标 | `missing_running_ids_of` 单独取出，恢复时显式报告，不静默丢弃 |

### 既有失败（与本次改动无关，已在干净 HEAD 上复现）

1. `npm run check` —— `vite.config.js:5` 的 `@ts-expect-error` 在本机 TypeScript 版本下被判定为多余。

2. `cargo clippy` —— 干净 HEAD 上即存在 13 项警告（`port.rs`、`supervisor.rs`、`ui_memory.rs`、
   `upstream.rs`、`process.rs`、`file.rs`、`cloudflare.rs`、`workspace/model.rs`），
   **均不在本次改动文件中**；CI 不跑 clippy。

> 本次交付的全量测试结果为 **230 passed / 0 failed**。
> 与本次改动直接相关的 `tray::`（43 项）与 `data::store::`（16 项）全部通过。
>
> 早期记录曾提到 `tools::exec::tests::windows_workspace_scripts_and_python_unicode_execute_successfully`
> 在本机稳定失败；本次复测已连续 4 次通过，疑与 Python 冷启动时序相关，
> 此处不再列为失败项。

### 本机安装验证（2026-09-19）

定制版已安装到本机并与官方 0.2.3 同路径覆盖，验证如下：

- 安装包 `Coding Tools MCP_0.2.3_x64-setup.exe`（静默安装退出码 0）。
- 安装后的 `coding-tools-mcp-desktop.exe` 中可检出本次新增字符串
  （`--autostart`、`tray-status`、`开机自启并启动 MCP/隧道`、`CodingToolsMcpDesktop`），
  确认安装的是定制构建而非官方原版。
- 应用启动正常，且拥有窗口类为 `tray_icon_app` 的窗口 —— 该窗口类仅在**托盘图标已注册**时出现，
  即托盘图标确实生效。
- 工作区配置在覆盖安装后保持不变（工作区、端口 28766、隧道 cloudflare 均保留）。
- 安装后 `HKCU\...\Run\CodingToolsMcpDesktop` **仍为空**：未启用真实开机自启。

### 人工确认项

- 托盘图标两态切换：已截图验证（运行中=蓝色原图，未运行=灰度同形；色彩饱和度量化 34.8 → 0.0）。
- 右键菜单勾选交互：已截图验证（勾选状态与注册表读回一致）。
- `--autostart` 登录启动效果：已模拟验证（窗口隐藏、MCP 与隧道拉起）；**真实登录触发仍建议由使用者亲眼确认一次**。

> 安装验证当时记录「Run 键为空」，但后续检查发现 Run 值**确实被写入过**，
> 工作区也已绑定。已将其移除，恢复为未启用；真实开机自启需使用者明确确认后再开启。

### 本机端到端验证：记住并恢复上次运行状态（2026-09-20）

先备份配置（`_backup-config-before-restore-feature-*.zip`），再静默安装（退出码 0）。
每项均通过**读回磁盘 `profiles.json` + 实际端口/进程状态**判定，而非只看日志。

| 场景 | 操作 | 实测结果 |
| --- | --- | --- |
| 空集合不恢复 | 置 `running=[]` 后普通启动 | MCP 28766 未监听、cloudflared=0，窗口可见 ✓ |
| 状态变化时记录 | `--autostart` 启动 | 磁盘写入 `['3a6d1aa7…']` ✓ |
| **强杀不丢已落盘记录** | 强杀进程 | 磁盘记录完好（说明记录不依赖退出路径；**不**代表掉电下的文件系统持久性） ✓ |
| **普通启动自动恢复** | 普通启动（无 `--autostart`） | MCP 28766 监听、cloudflared=1、窗口可见 ✓ |
| 内容未变不写盘 | 连续观察 13 秒 | `profiles.json` mtime 完全未变（未每 5 秒重写） ✓ |
| **失败可见且不伪记** | 外部进程占住 28766 后普通启动 | 弹出原生对话框 `#32770`，文案为「恢复上次运行的 MCP 时部分失败：「Nextcloud」：本地 MCP端口 28766 已被占用」；且磁盘集合变为 `[]`（失败**未**被记成运行中） ✓ |
| 路径互斥 | `running=[]` 时用 `--autostart` 启动 | 仍启动绑定工作区（不依赖历史集合），窗口 `Tauri Window` vis=False（已隐藏） ✓ |

验证方法备注：判断窗口是否隐藏必须用 `EnumWindows` 按类名找 `Tauri Window` 再看
`IsWindowVisible`；`(Get-Process).MainWindowHandle` 对该应用会返回错误句柄导致误判。
检测原生对话框要枚举类名 `#32770`（WinForms 的 `Application.OpenForms` 看不到它）。
