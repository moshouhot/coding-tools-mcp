# Phase 2 静态审查整改证据（2026-09-26）

## 状态

**CODE / STATIC / AUTOMATED: PASS**  
**REAL CHATGPT LIVE ACCEPTANCE: PENDING**

本文件记录 `docs/verification/phase2-static-review-2026-09-26.md` 中三项 P1 问题经用户审批后的整改结果。原问题报告及原始失败日志保持不变，不用本文件覆盖或改写历史证据。

## 基线与提交

- Phase 2 原实现基线：`074994870b8565d192e37bc926cfafe02e8f5601` (`0.2.4-custom.4`)
- 小修提交：`e19b24086635281b2398f035cb872bcd25af24c7`
- 原静态审查证据提交：`afa476bff3abfd0dafa65b748271ecb9d78d7ced`
- 三大问题整改实现：`b3fa0b5` (`fix-phase2-project-context-safety`)
- 整改候选版本：`0.2.4-custom.5`
- 整改分支：`fix/phase2-static-review`

当前仍未把本轮整改宣告为真实产品验收 PASS；合入 `custom/main` 并生成 NSIS 后，还需要真实 ChatGPT / Codex 新会话验证。

## P1-HISTORY-TARGET：旧检查点跨项目误写

### 原问题

同一 ChatGPT 会话先绑定 Alpha、再绑定 Beta 时，两边都可能返回：

```text
session_key = 同一个宿主会话 ID
current_path = docs/history-session/1.md
```

旧实现没有项目 identity。把 Alpha 的旧 checkpoint 目标交给已经切到 Beta 的会话，会静默写进 Beta，且返回 `ok=true`。

### 整改

History bootstrap 现在额外返回：

```text
project_id = sha256:<canonical project root identity>
```

`history_session_checkpoint` 的公开 schema 现在要求同时提供：

```text
session_key
expected_path
project_id
```

checkpoint 会把传入的 `project_id` 与当前 Active Project 的项目 identity 比对。不一致时：

```text
SESSION_PROJECT_MISMATCH
```

并且不会修改当前项目的历史文件。

MCP instructions 与桌面端 ChatGPT Session Prompt 同步要求：

1. 首次请求指定项目时先 `set_active_project`；
2. 再为该项目 `history_session_bootstrap`；
3. 保存 `session_key + current_path + project_id`；
4. 同一对话切换项目后，先为新项目 bootstrap/resume History，不沿用旧项目 checkpoint 目标。

### 回归证据

正式回归：

```text
old_checkpoint_is_rejected_after_project_switch ... ok
```

验证点：

- Alpha/Beta 的 `session_key` 可以相同；
- Alpha/Beta 的相对 `current_path` 可以相同；
- `project_id` 必须不同；
- Alpha 旧目标在 Beta 上 checkpoint 返回 `SESSION_PROJECT_MISMATCH`；
- Beta 历史文件字节保持不变。

## P1-HISTORY-BASELINE：History 自己写文件触发 Harness 外部修改

### 原问题

Harness Task 创建代码基线后，Coding Tools MCP 自己执行正常 History checkpoint，会改变项目内 `docs/history-session/*`。旧 Harness 把这些 MCP 自维护文件也算进代码 baseline，导致下一条 Exec/Patch 被错误判成：

```text
FILE_CHANGED_EXTERNALLY
```

### 整改

Harness baseline 现在支持 `ignored_paths`：

- 默认排除项目内 `docs/history-session`；
- History 使用自定义 `history_dir` 时，实际解析出的目录会登记为 MCP managed project path；
- managed History 目录会持久化，应用重启后仍继续排除；
- 不采用“History 写完就重拍整个 baseline”的危险方案。

因此 History 自维护变化不会阻断项目操作，而真实业务代码的外部变化仍保持原 Harness 门禁。

### 回归证据

```text
history_checkpoint_does_not_block_next_project_command ... ok
custom_history_directory_is_excluded_from_harness_baseline ... ok
custom_history_exclusion_survives_context_recreation ... ok
real_external_code_change_is_still_blocked_after_history_checkpoint ... ok
```

最后一条是反向证据：History 写入后再外部修改真实业务文件，下一条 Exec 仍返回 `FILE_CHANGED_EXTERNALLY`。整改没有关闭原有外部修改保护。

## P1-BINDING-LOSS：Session Active Project 丢失后静默 fallback

### 原问题

原 Active Project 映射只保留 256 条，采用插入顺序淘汰。老聊天绑定 Alpha 后，累计其他会话达到上限，Alpha 映射可能消失；随后该老聊天会静默使用 `default_cwd`（例如 Beta），导致读取或修改错误项目仍返回成功。

此外，原映射只在内存里，应用重启也会丢失。

### 整改

Session → Active Project 改为轻量本地持久化：

- 不再使用 256 条 FIFO 淘汰；
- `set_active_project` 同步持久化绑定；
- ToolContext / MCP 服务重建后恢复原聊天绑定；
- 状态写入使用 temp + backup；主文件丢失/损坏时可从 `.bak` 恢复；
- 已绑定项目目录被删除、改名或逃出 Workspace Pool 时，返回：

```text
ACTIVE_PROJECT_UNAVAILABLE
```

- 不允许静默 fallback 到 `default_cwd`；
- `set_active_project` / `discover_projects` 等恢复工具不依赖旧项目 Harness，因此可以显式重绑；
- 若持久化状态损坏，返回 `ACTIVE_PROJECT_STORE_UNAVAILABLE`。某个会话显式重绑后只恢复该会话，其他未知旧会话继续 fail-closed，避免错误 fallback。

### 回归证据

```text
many_other_sessions_do_not_evict_existing_project_binding ... ok
active_project_binding_survives_context_recreation ... ok
active_project_binding_recovers_from_backup_state_file ... ok
corrupt_binding_store_fails_closed_for_unrebound_sessions ... ok
missing_bound_project_fails_closed_until_explicit_rebind ... ok
```

## 同一工具调用的项目上下文一致性

静态审查还指出：一次调用中路径解析、Harness、History、Audit 原来可能多次读取可变 Session → Project 映射。

整改后工具入口先解析一次有效参数并附带内部 Active Project snapshot；后续 Harness 与 History 使用同一 snapshot。`call_tool_with_audit` 与真实执行共用同一份 prepared arguments，不再分别重新解析项目上下文。

这避免极端并发切换项目时出现：

```text
文件路径按 Project A
Harness / History / Audit 却按 Project B
```

的理论竞态。

## 原静态审查小修继续保留

以下三项 `e19b240` 修正继续作为正式回归：

1. 搜索省略 `path` 时默认当前 Active Project，而不是 Workspace Pool；
2. Codex `*** Add/Update/Delete File:` 与 unified diff 都正确按 Active Project 定位，不误改 Pool 根同名文件；
3. 当前项目目录被改名/删除后，项目管理工具仍可查看、发现、重绑项目。

## 自动化验证结果

最终 `0.2.4-custom.5` 本地发布门禁：

| 检查 | 结果 |
| --- | --- |
| `cargo check` | PASS |
| `cargo test --locked` | PASS |
| Rust lib unit tests | 240 passed / 0 failed |
| `call_tool_contract` | 31 passed / 0 failed |
| `call_tool_security` | 24 passed / 0 failed |
| `exec_path_regression` | 6 passed / 0 failed |
| `harness_state` | 4 passed / 0 failed |
| `harness_tool_contract` | 9 passed / 0 failed |
| `history_session` | 16 passed / 0 failed |
| `phase2_static_review` | 20 passed / 0 failed / 0 ignored |
| Rust 总计 | 350 passed / 0 failed / 0 ignored |
| `npm exec --yes npm@10.8.2 -- ci` | PASS |
| `npm run check` | PASS, 0 errors / 0 warnings |
| `git diff --check` | PASS |

Phase 2 审查中原先三个 `pending_*` 红色用例已经改成正常永久门禁，不再使用 `#[ignore]` 掩盖未解决问题。

## 保持不变的边界

- 一个 Connector + Workspace Pool 的产品方向不变；
- Safe / Trusted / Dangerous 权限逻辑未因本轮整改改变；
- 中文三级模式仍为“安全受限 / 受信任 / 完全放开”；
- 不迁移、不覆盖旧 History 文件；
- 不要求每个项目新建 Connector；
- 日常 standalone 模式不强制创建 Harness Task；
- 第一阶段 Active Project MVP 实机验收证据文件保持原样。

## 下一步真人验收

本轮代码和自动化可以进入 NSIS 候选构建，但还不能把 Phase 2 产品级状态写成 FINAL PASS。

安装候选版后至少验证：

1. 新 ChatGPT 对话明确指定项目时，先绑定 Active Project，再 bootstrap 项目 History；
2. bootstrap 返回 `history_scope=active_project`、正确 `project_root` 和 `project_id`；
3. History 真实写入该项目自己的 `docs/history-session`；
4. 同一聊天切换到第二个项目，重新 bootstrap 后两项目 History 不串；
5. 尝试沿用旧项目的 checkpoint 目标时明确拒绝，而不是写进新项目；
6. 重启 Coding Tools MCP 后，已有聊天的 Active Project 仍能恢复；
7. 另一个聊天绑定不同项目后，两边仍保持独立。

注意：`history_session_checkpoint` schema 新增必填 `project_id`。安装新版后 ChatGPT Connector 需要“刷新工具”，并建议新建对话进行本轮验收，避免旧会话继续持有旧工具 schema。

