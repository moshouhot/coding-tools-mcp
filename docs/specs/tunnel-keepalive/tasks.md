# 任务清单：tunnel-keepalive

## 概述

实现隧道/服务的可追溯日志、真实在线判据与有限保活恢复。需求见 `requirements.md`，
方案见 `design.md`，诊断证据见 `docs/verification/tunnel-keepalive-diagnosis-2026-10-18.md`。

> **边界**：本次**未**定位历史崩溃原因，交付的是后续可追溯能力与恢复机制。

## 交付物

| 文件 | 类型 |
| --- | --- |
| `src-tauri/src/lifecycle.rs` | 新建 |
| `src-tauri/src/tunnel/readiness.rs` | 新建 |
| `docs/specs/tunnel-keepalive/{requirements,design,tasks}.md` | 新建 |
| `docs/verification/tunnel-keepalive-diagnosis-2026-10-18.md` | 新建 |
| `src-tauri/src/tunnel/{mod,cloudflare,supervisor,access}.rs` | 修改 |
| `src-tauri/src/runtime/supervisor.rs` | 修改 |
| `src-tauri/src/{lib,commands/runtime}.rs` | 修改 |

## 实施项

- [x] 生命周期日志：启动/退出/panic/服务启停；1 MiB × 4 份轮转
- [x] 日志脱敏：`--token`/`secret=`/`Authorization:`/裸 `eyJ...`；修掉分隔符处理导致的值泄露
- [x] Cloudflare metrics 端口固定（`--metrics 127.0.0.1:<hash端口>`）
- [x] `/ready` 判据：200 且 `readyConnections > 0` 才算在线
- [x] 无法解析响应判为未就绪（不乐观）
- [x] 健康循环接入就绪探测；离线只记录不重启
- [x] `TunnelStatus.ready/ready_detail`，`state` 语义不变
- [x] `had_listening` 区分「启动失败」与「运行后意外死亡」
- [x] 保活：3 次上限 + 60s 冷却 + 选中即计次 + 不持锁跨越 await
- [x] 保活复用 `start_by_kind`，与手动启动同一路径

## 验证

| 项 | 结果 |
| --- | --- |
| `cargo test --locked --lib` | 263 passed / 0 failed |
| 就绪判定单测 | 12 项（含隔离注入：503/0、200/0、200/4、无监听、不可解析） |
| 脱敏单测 | 5 项（含多 token、裸 eyJ、密钥字段、普通文本保留） |
| 保活安全边界单测 | 5 项（未监听不恢复 / 运行后死亡恢复 / 冷却 / 上限 / 用户停止不复活） |
| `cargo clippy --lib` | 15（基线 15，未新增类别） |
| 实机：生命周期日志 | `lifecycle.log` 真实落盘，含启动与服务启动记录 |
| 实机：metrics 端口 | `--metrics 127.0.0.1:20525` 与哈希预期一致 |
| 实机：在线判定 | 隧道恢复后 `/ready` = `200 + readyConnections:4`，判为在线 |
| 实机：离线形态 | 故障期 `/ready` = `503 + readyConnections:0`，判为离线（单测覆盖同一响应） |

## 已知未覆盖

- `harness_tool_contract::无任务时_exec_command不返回任务门禁错误` 失败。
  已在 **HEAD（回退本次改动）** 上复现同样失败，**非本次引入**；
  属 `exec_command` 在等待窗口内报 `running` 的既有问题，另行处理。
- 保活未覆盖主进程被强杀的情形（需外部 watchdog，明确不在范围）。
- `spawn_cloudflare_tunnel` 参数 9/7 触发 clippy `too_many_arguments`
  （改动前已是 8/7 的既有警告，类别未新增）。
