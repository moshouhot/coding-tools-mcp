# Phase 2 静态复查与隔离回归（2026-09-26）

## 结论与版本

**REVIEW: CHANGES REQUIRED。第二阶段真实项目验收暂缓。**

- 审查基线：`074994870b8565d192e37bc926cfafe02e8f5601`，`0.2.4-custom.4`。
- 上一阶段证据提交：`8eff9965d16ea5dacb6bb24232afd733e7bfd965`，未修改。
- 本轮审查分支：`fix/phase2-static-review`。
- 实现级小修提交：`e19b24086635281b2398f035cb872bcd25af24c7`。
- 已修三类实现缺陷；另有三项重大行为问题等待用户审批，不能用已有测试全绿代替验收。
- 本轮未合并 `custom/main`、未推送、未升级安装器、未替换正在使用的 MCP 程序。读取服务信息时运行版本仍为 `0.2.4-custom.3`。

设计目标不变：一个 Connector 管理 Workspace Pool；用户自然语言指定目录；不同对话独立绑定项目；History/Harness 跟随项目；不新增日常命令白名单或确认门槛；不迁移或覆盖旧历史。

## 方法与证据边界

先读真实 Git 状态和 `8eff996..0749948` 差异，再复核 History、Harness、项目上下文、工具分发和补丁解析调用链。新增复现全部使用 `tempfile::TempDir`，Harness 存储也在该临时目录；没有对 uTools 等真实业务项目执行这批复现。

调用了仓库指定的 mcp-probe-kit `code_review` 收集差异，并按 AGENTS 要求执行 GitNexus 影响分析。图谱工具只提供影响范围，不是第二位 AI 的语义审计，也不构成产品通过证明。托管 GitNexus 1.6.9 的索引仅供本次复核，不随本次提交修改 AGENTS/CLAUDE 或依赖版本。

证据目录：`docs/verification/phase2-static-review-2026-09-26/`。
回归代码：`src-tauri/tests/phase2_static_review.rs`。

## 已直接修复的实现问题

### F-01：省略搜索路径时，搜索范围回到了整个 Pool

基线位置：`src-tauri/src/tools/dispatch.rs:354-358`。

`search_text/grep_text/grep` 仅在参数中已有 `path` 时才补项目路径；调用 `search_text({query: ...})` 时，底层默认 `.` 是 Pool，不是当前项目。

隔离复现：Pool、Alpha、Beta 各放一个相同标记；绑定 Alpha 后省略 path 搜索，基线会命中三处。

修复：搜索工具与目录工具一样，先采用 `path.unwrap_or(".")`，再按当前项目补路径。显式路径与既有权限规则不变。三个别名均有回归覆盖。

### F-02：Codex 补丁信封未补项目路径，可能修改 Pool 同名文件

基线位置：`src-tauri/src/tools/dispatch.rs:377-391`；解析位置 `src-tauri/src/tools/patch.rs:227-266`。

旧 `prefix_patch_paths` 只识别 `--- a/`、`+++ b/`，不识别 `*** Add/Update/Delete File:`。绑定 Alpha 后，常见 Codex 补丁仍以 Pool 为根；若 Pool 存在同名文件，就会实际更新/删除错误文件，而不只是报找不到路径。

修复：区分 Codex 信封与 unified diff；只改对应格式的文件头，不改补丁正文。补全无 a/b 前缀的 unified 文件头，保留 `/dev/null`；Codex 文件路径不再错误删除真实 `a/`、`b/` 目录名。

回归覆盖新增、更新、删除、预检不落盘、正文类似 unified 文件头、真实 a/b 目录、两种 unified 头和父目录越界拒绝。

### F-03：旧项目改名后，新项目也切不过去

基线位置：`src-tauri/src/tools/dispatch.rs:67-72`；`src-tauri/src/tools/context.rs:209-222`。

每个工具在分发前都构造旧项目 Harness。旧目录改名后，`set_active_project("beta")` 先因旧根不存在报 `WORKSPACE_UNAVAILABLE`，无法执行恢复操作。

修复：权限校验仍先执行；随后让 `server_info/get_active_project/set_active_project/discover_projects` 不依赖旧项目 Harness。正常代码操作仍走原有 Harness 流程。

这些是现有契约的实现修正，不是新增项目管理规则。产品源代码只改了 `dispatch.rs` 和 `patch.rs`；权限策略、模式中文、Workspace 配置、版本号、History/Harness 状态规则未改。

## 等待审批的重大问题

### P1-HISTORY-TARGET：旧检查点可能静默写入另一个项目

代码：`src-tauri/src/tools/history/mod.rs:213-244,724-742`；初始化说明：`src-tauri/src/mcp/server.rs`。

复现步骤：同一宿主会话先绑定 Alpha 并 bootstrap，再绑定 Beta 并 bootstrap。两个项目都从 `docs/history-session/1.md` 开始编号，返回的 `session_key` 和 `current_path` 完全相同。随后提交从 Alpha 保存下来的这两个参数，实际写入 Beta 的历史，返回 `ok=true`，且没有警告。

这违反“把 bootstrap 返回的稳定目标原样交回”的约定。原因是目标只包含会话键和项目内相对路径，真正的根目录却每次从可变 Active Project 重新取得。若切换后不重新 bootstrap，则可能报 `SESSION_NOT_BOOTSTRAPPED`；当前“整个对话只初始化一次”的文案也不完整。

**建议审批：** 给历史目标加入可验证的项目身份，或使用 Pool 内唯一的档案路径；检查点必须校验完整目标，不能根据当前项目静默重新解释旧目标。切换项目后初始化/恢复该项目自己的档案；切回原项目恢复原档案。保留旧文件，不自动搬迁、不合并不同项目历史，也不要求新 Connector。

用例：`pending_old_checkpoint_must_not_silently_write_to_another_project`。已实际执行，失败；Beta 文件确实被改变。

### P1-HISTORY-BASELINE：自己保存历史，会让后续命令被当作外部修改挡住

代码：`src-tauri/src/harness/state.rs:156-171,500-548,580-617`；`src-tauri/src/tools/dispatch.rs` 的 `requires_write_baseline` 调用及 History 分发。

复现：绑定 Alpha → bootstrap → `start_task` → 正常 `history_session_checkpoint` → `exec_command("pwd")`。未做其他代码修改，仍得到 `FILE_CHANGED_EXTERNALLY`。在 `dangerous` 下同样复现。

原因是 Harness 基线包含项目内 History 文件，而 History 工具的写入没有与该基线协调。这个问题需要活动 Harness Task 才触发；纯 standalone、没有活动任务时不会因此拦住命令。

**建议审批：** 将服务维护的历史元数据与代码基线分开处理，覆盖实际配置的历史目录；保留真实代码外部变更检测。不能简单在每次保存历史后全量重置基线，否则会把同时发生的外部代码修改一并认作可信。日常 standalone 继续不要求创建任务。

用例：`pending_history_checkpoint_must_not_block_next_project_command`。已实际执行，失败，错误为 `FILE_CHANGED_EXTERNALLY`。

### P1-BINDING-LOSS：会话绑定被淘汰后，工具可能静默转到全局默认项目

代码：`src-tauri/src/tools/context.rs:12,29-43,172-187`。

这是第一阶段遗留、第二阶段会继承的问题。映射最多存 256 项，按插入顺序淘汰，读取活跃会话不会更新顺序。不是必须同时打开 257 个窗口；长期累计不同会话也可触发。

复现：当前会话绑定 Alpha，全局 default_cwd 为 Beta；写入另 256 个不同会话绑定后，当前会话的 `get_active_project` 返回 `source=default_cwd`、Beta；下一次 `read_file("name.txt")` 实际读到 Beta 内容，且 `ok=true`。

**建议审批：** 区分“从未绑定”和“原绑定丢失”，不能把后者静默当作前者处理。绑定丢失时明确通知 AI 用当前对话原项目路径重绑，不要求用户重新添加 Connector；是否持久化每会话映射需明确设计。同步把每次请求的项目上下文固化为一份快照，供路径、History、Harness、审计使用，避免同一调用中多次查询可变映射。

用例：`pending_evicted_binding_must_not_silently_read_a_different_project`。已实际执行，失败，读取结果为 Beta。

## 补充静态边界

- `call_tool_with_audit → apply_default_cwd → call_tool → harness_for_session → history::resolve_scope` 会多次读取会话映射；上面“单请求快照”建议也针对该一致性风险，本轮没有做真实并发压力证明。
- `resolve_scope` 和 `harness_for_session` 重新 canonicalize 已绑定目录时，缺少统一的 Pool 包含关系复核。目录后续被替换为链接的情形需要随快照解析一起覆盖；本轮未对真实目录做链接替换实验。
- Harness 的 operation log 已按项目 identity 分开，不等于 SQLite 工具审计已按项目分区；后者仍以 Workspace profile 为主。本轮不顺手修改审计数据库结构。
- 两个对话选择同一个项目时会看到同一项目的 Harness Task；这与“两对话操作不同项目”是不同场景，不宣称同项目并发写入自动隔离。
- 当前 `.3` 执行桥对一次带引号的组合 shell 提交命令返回了 Git pathspec 错误；拆为单独 `git commit` 后成功。该现象未被本轮两处源代码修复覆盖，不将其归因于 History/Harness，也未把失败的第一次提交报告成成功。

## 测试结果：必须同时读绿色与红色两部分

| 检查 | 真实结果 |
| --- | --- |
| 修改产品代码前，初始 8 个针对性用例 | 8 failed，退出码 101；证明旧测试漏掉了这些行为 |
| 小修后，新增常规回归 | 10 passed，3 pending 用例默认 ignored |
| 显式运行 3 个 pending 用例 | 3 failed，退出码 101；不是未执行，也不是已经修好 |
| 完整 `cargo test --locked` | 340 passed，3 ignored，退出码 0 |
| `npm run check` | 0 errors / 0 warnings，退出码 0 |
| `git diff --check` | 通过 |
| GitNexus 修改前影响分析 / 提交前 detect-changes | 已运行；分发入口影响面高，真实产品修改限于两个文件 |

普通全量回归绿色只证明小修没有让现有测试新增失败。三个设计用例已明确命名为 pending 并附 ignore 原因，显式失败日志保存在证据目录，因此**本记录绝不把第二阶段判为 PASS**。

复验命令：

```text
cargo test --locked --test phase2_static_review
cargo test --locked --test phase2_static_review pending_ -- --ignored --test-threads=1 --nocapture
```

第一条应通过常规小修用例；第二条在重大问题批准并修复之前应失败。所有新增用例均操作临时测试目录。

原始日志使用子进程文件描述符直接捕获，保留 UTF-8 输出；各日志摘要和 source commit 见证据目录 manifest。初始 8 用例采集时的测试文件指纹是时点记录；最终提交的测试文件扩展为 13 用例并经过格式化，二者不声称字节相同。

## 发布与下一步

本轮只在审查分支固化小修和证据。`custom/main` 不前移，不触发新安装包，不覆盖当前运行程序。需先由用户审批上述三项行为修正；修复并复核后，再通知用户进行双对话 History/Harness 真实验收。

此前双对话 A→B→A 的成功截图仍证明其所测场景；本次发现说明不能将那组窄场景证据扩大成“所有补丁、生命周期和历史写入边界均已验证”。第一阶段证据文件保持原样。
