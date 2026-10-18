# 设计文档：tunnel-keepalive

## 概述

把"服务或隧道不声不响地死掉"变成可追溯、可观测、可自愈。
证据见 `docs/verification/tunnel-keepalive-diagnosis-2026-10-18.md`。

**本次诊断未找到历史崩溃原因**（无 WER、无事件日志）。本设计提供的是后续
可追溯的证据与恢复能力，**不声称**已定位或解决那次未知崩溃。

## 技术方案

### 1. 应用生命周期日志（`src-tauri/src/lifecycle.rs`）

- 位置：`<app_config_dir>/logs/app/lifecycle.log`
- 内容：启动（版本/PID）、正常退出、Rust panic、服务启停与保活结果。
- 轮转：单文件 1 MiB，保留 4 份（`lifecycle.log` + `.1`..`.3`）。
- 脱敏：`--token`/`token=`/`secret=`/`Authorization:` 等字段与裸 `eyJ...` blob
  一律替换为 `[REDACTED]`。**不写完整命令行**。
- 写失败只降级到 stderr，绝不 panic 或阻断主流程。

**边界**：Rust panic hook 只能捕获可展开的 panic。原生崩溃（访问违例等）
不会走到这里，需要 WER/minidump —— 不在本次范围。

### 2. 隧道在线判据（`src-tauri/src/tunnel/readiness.rs`）

**核心原则：进程存在 ≠ 隧道在线。**

现场形态：cloudflared 活着、本地 28766 正常，但 `/ready` 返回
`503 + readyConnections:0`，公网 530（error code 1033）。

- 启动时用 `--metrics 127.0.0.1:<port>` **固定**端口。
- 端口由 workspace id 的 FNV-1a 哈希稳定映射到 `20251..20603`，
  保证同一工作区每次一致，且不同工作区不互抢。
- 判定：`/ready` HTTP 200 **且** `readyConnections > 0` → 在线；其余离线。
- 无法解析的响应判为**未就绪**，绝不乐观当成在线。

### 3. 离线不重启风暴

`ensure_frp_health_loop`（15s）中新增 Cloudflare 就绪探测：
探测到离线**只记录、只上报**，不杀 cloudflared。
理由：cloudflared 自身已按 32s 退避重连；边缘不可达时反复杀进程
只会在网络恢复瞬间制造重启风暴。

### 4. 服务保活（`runtime/supervisor.rs` + `lib.rs`）

区分两类失败：

| 情况 | 判据 | 是否恢复 |
| --- | --- | --- |
| 启动就失败（配置错、端口冲突） | `had_listening == false` | **否**（否则无限重试） |
| 曾正常运行、随后意外死亡 | `Error` 且 `had_listening == true` | **是** |
| 用户显式停止 | `Stopped` | **否** |
| 程序退出 | 进程结束，循环随之结束 | — |

- `had_listening` 在观测到端口真的监听成功时置位，每次启动重置。
- 恢复上限：每个服务 3 次（`MAX_KEEPALIVE_ATTEMPTS`）。
- 冷却：两次尝试间隔 ≥ 60s（`KEEPALIVE_COOLDOWN`）。
- `take_keepalive_targets` **选中即计次**，并发调用不会重复启动。
- 循环 30s 一轮；取目标后立即释放 runtime 锁，**不持锁跨越 await**。
- 复用 `commands::runtime::start_by_kind`，与用户手动启动同一套逻辑。

**不用 `running_mcp_workspace_ids` 当意图**：那是"实际观测状态"，
用于下次启动恢复；保活意图来自 `RuntimeSupervisor` 的 phase。

### 5. 状态如实上报

`TunnelStatus` 新增 `ready: Option<bool>` 与 `ready_detail: Option<String>`。
`state` 语义保持不变（是否被管理且进程存活），避免影响既有
`state == "running"` 判断（5 处调用点）；在线与否必须看 `ready`。

## 明确不做

- 外部 watchdog / Windows 服务 / 计划任务：主进程被强杀时进程内定时器无法自救。
- 修改防火墙 / 代理 / 网络配置。
- 边缘不可达时杀 cloudflared 重启。
- minidump 采集。

## 文件结构

| 文件 | 变更 |
| --- | --- |
| `src-tauri/src/lifecycle.rs` | 新建：日志 + 轮转 + 脱敏 |
| `src-tauri/src/tunnel/readiness.rs` | 新建：`/ready` 判定 + metrics 端口分配 |
| `src-tauri/src/tunnel/mod.rs` | 声明并导出 readiness |
| `src-tauri/src/tunnel/cloudflare.rs` | spawn 增加 `--metrics <port>` |
| `src-tauri/src/tunnel/supervisor.rs` | 就绪缓存、`TunnelStatus.ready`、探测方法 |
| `src-tauri/src/tunnel/access.rs` | 健康循环接入就绪探测（不重启） |
| `src-tauri/src/runtime/supervisor.rs` | `had_listening`、`take_keepalive_targets` |
| `src-tauri/src/commands/runtime.rs` | `start_by_kind` + 启停记录 |
| `src-tauri/src/lib.rs` | panic hook、启动/退出记录、保活循环 |
