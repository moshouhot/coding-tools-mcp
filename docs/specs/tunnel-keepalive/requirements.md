# 需求文档：tunnel-keepalive

## 功能概述

让"隧道/服务不声不响地死掉"变成**可追溯、可观测、可自愈**：

1. **可追溯**：持久化、轮转的应用生命周期日志，事后能回答"上次为什么没了"。
2. **可观测**：区分「进程在」与「隧道真的在线」，离线状态如实显示。
3. **可自愈**：仅对**意外退出**做重启，且必须尊重用户意图。

背景与证据见 `docs/verification/tunnel-keepalive-diagnosis-2026-10-18.md`。

> **重要边界**：本次诊断**没有**找到历史崩溃原因（无 WER / 无事件日志）。
> 本功能不声称"崩溃原因已解决"，只提供后续可追溯的证据与恢复能力。

## 范围边界

**In Scope**

- 应用生命周期日志：启动（版本/PID）、正常退出、panic、服务退出与恢复结果；轮转 + 限额；脱敏。
- Cloudflare 隧道在线判据：基于固定端口 metrics `/ready`，而非仅 pid 存活。
- 离线状态如实上报（不把"进程在"当"隧道在线"）。
- 服务意外退出后的恢复：退避、冷却、单次恢复，尊重用户意图。
- 主进程 panic 记录（`std::panic::set_hook`）。

**Out of Scope（明确不做）**

- 外部 watchdog / Windows 服务 / 计划任务（主进程被强杀无法自救）。
- 修改防火墙、代理、网络配置。
- 在边缘不可达时反复杀 cloudflared 重启（cloudflared 自身已在退避重连）。
- 用宏观"保活"掩盖未定位的历史崩溃。
- 原生崩溃转储（minidump）的采集。

## 术语定义

- **在线（ready）**：cloudflared metrics `/ready` 返回 200 且 `readyConnections > 0`。
- **离线**：进程存在但 `/ready` 非 200 或 `readyConnections == 0`。
- **意外退出**：服务进程退出，且用户未请求停止、未退出程序、未删除工作区。
- **运行意图**：用户明确要求某服务运行。**不得**用 `running_mcp_workspace_ids`
  （那是实际观测状态）代替。

## 需求列表

### FR-1: 应用生命周期日志

**优先级:** Must

1. WHEN 应用启动 THEN 系统 SHALL 记录版本、PID、启动时间到**持久化**日志文件。
2. WHEN 应用正常退出 THEN 系统 SHALL 记录退出原因。
3. WHEN 应用发生 Rust panic THEN 系统 SHALL 记录 panic 位置与消息（不记录等于崩溃转储）。
4. WHEN 服务启动/退出/恢复 THEN 系统 SHALL 记录结果（成功或失败原因）。
5. 日志 SHALL 按大小轮转并限制保留数量。
6. 日志 SHALL 脱敏：不得写入隧道 token、完整命令行、cookie、密钥。

### FR-2: 隧道在线判据

**优先级:** Must

1. 系统 SHALL 为 cloudflared 指定固定 metrics 端口（不依赖默认随机端口）。
2. WHEN 判定隧道是否在线 THEN 系统 SHALL 使用 `/ready` 的 HTTP 状态与
   `readyConnections`，**不得**仅以进程存活作为在线依据。
3. WHEN 进程存在但 `/ready` 未就绪 THEN 状态 SHALL 显示为离线/未就绪。

### FR-3: 离线不重启风暴

**优先级:** Must

1. WHEN cloudflared 进程存活但连接未就绪 THEN 系统 SHALL 如实标记离线，
   **不得**因此杀进程重启。
2. WHEN 多次探测仍离线 THEN 系统 SHALL 保持状态上报，不做无界重启。

### FR-4: 意外退出恢复

**优先级:** Must

1. WHEN 服务进程意外退出且该服务仍被要求运行 THEN 系统 SHALL 尝试恢复一次，
   并使用退避 + 冷却期避免风暴。
2. WHEN 用户手动停止该服务 THEN 系统 SHALL **不**恢复它。
3. WHEN 用户退出程序 THEN 系统 SHALL **不**恢复任何服务。
4. WHEN 工作区已被删除 THEN 系统 SHALL **不**恢复其服务。
5. WHEN 恢复进行中用户请求停止/退出/删除 THEN 系统 SHALL 可取消恢复。
6. 恢复 SHALL 遵守单实例约束：并发触发不得重复启动同一服务。

### FR-5: 状态如实上报

**优先级:** Must

1. WHEN 隧道离线 THEN 托盘状态/工具提示 SHALL 反映离线，而不是显示运行中。
2. WHEN 恢复失败 THEN 系统 SHALL 记录原因，且不得谎报为成功。

## 非功能需求

- **NFR-1**: 日志写入失败不得导致应用崩溃或拖垮主流程。
- **NFR-2**: 健康探测 SHALL 有超时，不得阻塞 UI 事件循环。
- **NFR-3**: 不得在持有同步锁时 `await`。
- **NFR-4**: 恢复策略必须可测试（注入时钟/后端），不能只靠真机。

## 依赖关系

- 现有 `TunnelSupervisor` / `access.rs` 的 FRP 健康循环（复用模式，不重写）。
- `platform().is_process_alive` 判进程存活。
- cloudflared 的 `--metrics` 参数（需确认本机版本支持）。
