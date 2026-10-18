# 故障诊断：隧道离线与"保活"诉求

> 记录时间：2026-10-18
> 结论先行：**本次查到的不是程序崩溃，而是 Cloudflare 边缘 TLS 握手持续超时，
> 导致隧道离线（公网 530 / error code 1033）。程序本身全程存活。**

## 一、先说清楚"能证明什么、不能证明什么"

| 判断 | 证据 | 可信度 |
| --- | --- | --- |
| 主进程当前存活 | 进程 15:12:07 启动，`Responding=True`，本地 28766 在监听 | 已证实 |
| 近 7～14 天**没有**本程序的崩溃记录 | `WER\ReportArchive/ReportQueue`、`CrashDumps` 无匹配；Application 日志 ID 1000/1001/1026 近 14 天无本程序条目 | 已证实（"没有记录"，不等于"从未崩溃"） |
| cloudflared 活着但**没有 edge 连接** | `/ready` 返回 `503 + readyConnections: 0`；公网 530 | 已证实 |
| 最后一次成功注册连接 | `2026-10-18T05:17:44Z`（本地 13:17） | 已证实 |
| 之后持续 TLS 握手超时 | 93 条 `ERR TLS handshake with edge error: read tcp ...:7844: i/o timeout` | 已证实 |
| 是"7844 端口被墙" | `Test-NetConnection region1.v2.argotunnel.com:7844` 返回 **True** | **未证实** —— TCP 可达 ≠ TLS 握手成功，不能用握手超时反推端口封锁 |
| 日志"四轮后放弃" | 日志末条 `07:29:21Z Retrying connection in up to 32s`，仍在重试 | **不成立**，此前判断有误 |

### 排查中纠正的三个误判（留档，避免重复踩）

1. **命令行截断导致误判**：先前只打印前 110 字符，看到 `--protocol http2` 就断言缺少
   `tunnel run`。完整命令行为
   `cloudflared.exe --protocol http2 --post-quantum=false tunnel run --token <...>`，
   命名隧道参数完整。
2. **530 含义误读**：响应正文是 `error code: 1033`。1033 是 Cloudflare 侧
   "隧道未连接"，**不是**鉴权失败、也不是本地 origin 不可达。
3. **"没有崩溃"证据强度**：只能说查不到 WER / 事件日志记录，
   不能据此断言历史上从未崩溃。

## 二、当前真实故障

```
本地 MCP 127.0.0.1:28766   →  正常监听（curl 本地通）
cloudflared 进程            →  存在（pid 变化过，父进程是本程序）
cloudflared ↔ CF edge      →  持续 TLS 握手超时，registered=0
公网 https://mcp.666606.xyz →  530 error code 1033
cloudflared 自身 pre-check  →  TCP 7844 FAIL / UDP 7844 FAIL / API 443 PASS
```

即 advisor 所述**第三层故障**：主进程活着、本地服务正常，但隧道没有 edge 连接。

## 三、现有"保活"能力的真实边界

| 服务 | 进程存活检测 | 健康探测 | 自动重启 | 网络恢复后重连 |
| --- | --- | --- | --- | --- |
| FRP (`frpc`) | 有（`session_is_running` + `is_process_alive`） | 有（15s 周期 `heal_unhealthy_frpc`，含公网探测） | 有 | 有（`restart_frpc_after_network_recovery`） |
| Cloudflare | 有（pid 存活） | **无** | **无** | **无** |

`ensure_frp_health_loop` 只覆盖 FRP。Cloudflare 侧既没有健康判定，也没有重启策略，
更不会把"进程在、但连接不在"识别为离线。

## 四、cloudflared 自己已经在重连

日志显示 cloudflared 自身按 32s 退避持续重试。因此**在边缘恢复可达之前，
反复杀进程重启没有意义**，只会在网络恢复瞬间制造重启风暴。

## 五、待实施的保活（见 docs/specs/tunnel-keepalive/）

1. 应用生命周期日志（持久化、轮转、脱敏），让"上次为什么没了"可追溯。
2. 用固定端口的 metrics `/ready` 做权威在线判据，替换"进程存在即在线"。
3. 离线时**只标记状态、不杀 cloudflared**，交给它自己重连。
4. 仅当进程真的退出时才重启，带退避与冷却，且必须尊重用户意图
   （手动停止 / 删除工作区 / 退出程序 不得复活）。
5. 不引入外部 watchdog / 系统服务 / 计划任务（本次范围外）。

## 六、本次未解决、也未掩盖的问题

- **历史崩溃原因仍未定位**。查不到崩溃记录，就没有证据可依。
  新增保活机制不能让这个问题"看起来解决了"。
- **TLS 握手为何超时**属网络环境问题，代码侧无法修复，
  只能做到"可观测 + 恢复后自动重连"。
- 若主进程本身被强杀，进程内定时器无法自救 —— 这需要外部 watchdog，
  明确不在本次范围。

## 七、附：token 泄露面

cloudflared 命令行包含 `--token eyJ...`（隧道 token 明文）。
任何读取进程命令行的地方都能看到它。日志脱敏必须覆盖命令行，
且不应把完整命令行写入日志。
