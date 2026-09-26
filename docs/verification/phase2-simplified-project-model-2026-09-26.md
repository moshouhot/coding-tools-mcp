# Phase 2 简化项目模型整改证据（2026-09-26）

## 结论

**CODE / STATIC / AUTOMATED: PASS**
**REAL CHATGPT LIVE ACCEPTANCE: PENDING**

本轮根据用户确认的真实使用习惯，把 Phase 2 从“一个会话可频繁切换多个项目”的通用模型收窄为：

> **一个 ChatGPT / Codex 对话默认只对应一个项目。**

其他项目可以通过完整绝对路径做只读参考，但不会自动改变 Active Project；只有用户明确要求切换当前项目时，才允许显式重绑。

本文件记录 `.5` 第二轮静态复审中 6 个 RED 行为的整改结果，以及 `.6` 候选的最终本地自动化证据。此前 `.5` 的 RED 报告、探针、日志、manifest 和 advisor 输出全部保留，不回写、不覆盖。

## 版本与提交

- 原 `.5` 审查基线：`3adfb99f9f7fbc0ad029d7c835daa28da416441a`
- `.5` 最终复审 HOLD 证据：`a7ef61fa880354a1a754a8bfc0a24d70643ee2dd`
- `.6` 简化模型实现：`592aaa6a69bc11686c93031499dd7fec9587cc76`
- `.6` 跨项目 Exec 作用域硬化：`f47f8d8e9a0b1ca9d055ee15aace88b7a2ebdc9c`
- 候选版本：`0.2.4-custom.6`
- 实施分支：`fix/phase2-one-session-one-project`

当前真实运行中的 Connector 仍是旧版本；本轮 `.6` 只完成源码、静态、自动化与架构校验，尚未冒充真实 ChatGPT 实机验收。

## 用户批准的最终产品规则

1. **一个 host Session 默认只绑定一个 Active Project。**
2. 首次明确项目完整路径后自动绑定。
3. 已绑定后，即使对话中出现另一个项目的完整路径，也**不自动切换**。
4. 只有用户明确要求“切换当前项目”时，Agent 才能调用：

   ```text
   set_active_project(..., allow_rebind=true)
   ```

5. 带 host Session 的项目操作如果没有有效绑定，返回 `ACTIVE_PROJECT_REQUIRED`；绝不把 `default_cwd` 当成当前项目继续工作。
6. 其他项目完整路径可以用于只读参考；不会改变 Active Project。
7. `exec_command` 的 `workdir/cwd` 必须留在当前 Active Project；要去其他项目执行，先显式 rebind。
8. History 固定为：

   ```text
   <Active Project>/docs/history-session
   ```

   不再支持任意 `history_dir`。
9. 一个 Connector + Workspace Pool 的产品方向不变。

## `.5` 六个 RED 与 `.6` 整改映射

### R1：绑定存储损坏后修复其他 Session，老 Session 第二次重启可能静默 fallback

`.5` 问题：共享整表 JSON 损坏后，修复另一个 Session 会重写一份“正常但不完整”的状态；下一次重启时老 Session 已经没有损坏标志，可能落到 `default_cwd`。

`.6` 整改：

- host Session 无绑定直接 `ACTIVE_PROJECT_REQUIRED`；
- 不再存在 host Session → `default_cwd` 的隐式 fallback；
- 每个 Session 独立持久化，单个 Session 状态损坏不会被另一个 Session 的修复“洗白”。

永久回归：

```text
host_session_without_binding_never_falls_back_to_default_cwd ... ok
corrupt_one_session_binding_stays_closed_after_another_session_repairs_itself ... ok
```

### R2：多个 ToolContext 共用一个整表 JSON，后写覆盖先写

`.5` 问题：两个上下文各自持有旧内存 map，顺序保存也可能发生 lost update。

`.6` 整改：

- 删除“一个 JSON 保存所有 Session”的模型；
- 改为：

  ```text
  active-project-sessions/<workspace-id>/<sha256(session-key)>.json
  ```

- Session A 只写 A 的文件，Session B 只写 B 的文件，天然隔离。

永久回归：

```text
separate_contexts_do_not_clobber_other_session_bindings ... ok
```

### R3：`history_dir=src` 可以把真实源码排除出 Harness baseline

`.5` 问题：History scope 解析会把调用者给出的目录登记为 MCP 自维护目录，因此只读 History 搜索也可能把 `src/` 变成 Harness 盲区。

`.6` 整改：

- History 固定到 `docs/history-session`；
- public History schema 删除 `history_dir`；
- 内部若仍收到旧参数且不是固定路径，返回 `HISTORY_DIR_FIXED`；
- 删除动态 `managed-project-paths` 登记逻辑。

永久回归：

```text
custom_history_path_cannot_exempt_real_source_code ... ok
```

该测试还验证：真正的源码外部变化仍返回 `FILE_CHANGED_EXTERNALLY`，不是通过关闭 Harness 来“修好”。

### R4：只读 History 搜索依赖可写元数据

`.5` 问题：`history_session_search` 只是读取，却会登记/持久化 managed path，因此元数据目录不可写时读操作也失败。

`.6` 整改：固定 History 后删除动态目录登记；只读搜索不再写这类 metadata。

永久回归：

```text
readonly_history_search_does_not_write_managed_path_metadata ... ok
```

### R5：Workspace 根项目 + 自定义 History 排除规则失效

`.5` 问题建立在“自定义 History 目录”上。

`.6` 通过收窄产品能力消除该组合：History 只有固定系统目录。即使测试中 Active Project 恰好等于 Workspace 根，正常固定 History 也不会和 Harness baseline 自冲突。

永久回归：

```text
workspace_root_project_uses_fixed_history_without_baseline_self_conflict ... ok
```

### R6：原绑定目录被替换成指向兄弟项目的链接后，Session 静默跟随

`.5` 只确认“当前 canonical 目标仍在 Workspace Pool 内”，没有确认它仍是最初绑定的项目。

`.6` 每个 Session binding 同时持久化：

```text
binding_path
canonical_target
```

每次使用都会重新 canonicalize `binding_path` 并与初次 `canonical_target` 比对；若同一路径已经改指别处，返回：

```text
ACTIVE_PROJECT_TARGET_CHANGED
```

永久回归：

```text
bound_path_retargeted_by_symlink_fails_closed ... ok
```

## 额外静态复查硬化：跨项目 Exec

在完成六项整改后的最后反向检查中，又发现一个小而明确的边界：`exec_command` 的绝对 `workdir/cwd` 原来可以指向 Workspace Pool 中的兄弟项目。

这与用户批准的“其他完整路径主要用于讨论/参考，不应偷偷改变执行项目”不一致，因此直接修复：

- `read_file` 等显式绝对路径只读引用其他项目：允许；
- Active Project 保持不变；
- host-session `exec_command` 的 `workdir/cwd` 如果逃出 Active Project：

  ```text
  ACTIVE_PROJECT_SCOPE_VIOLATION
  ```

- 想在另一个项目执行，必须先显式 rebind。

永久回归：

```text
cross_project_absolute_read_is_allowed_but_exec_workdir_is_not ... ok
```

## 绑定恢复与重绑规则

`.6` 还永久覆盖以下行为：

```text
project_switch_requires_explicit_rebind_flag ... ok
same_project_reselection_refreshes_a_stale_alias_path ... ok
```

含义：

- 一个已绑定 Session 不能因为第二个路径被提到就切项目；
- 明确切换需要 `allow_rebind=true`；
- 如果只是同一个真实项目换了合法路径别名，可以安全刷新 binding path，不需要把它当成跨项目切换。

## 自动化测试结果

最终源码 HEAD：`f47f8d8e9a0b1ca9d055ee15aace88b7a2ebdc9c`

### Rust

```text
cargo test --locked
```

最终结果：**362 passed / 0 failed / 0 ignored**。

主要套件：

| 套件 | 结果 |
| --- | --- |
| lib unit | 240 / 240 PASS |
| call_tool_contract | 31 / 31 PASS |
| call_tool_security | 24 / 24 PASS |
| exec_path_regression | 6 / 6 PASS |
| harness_state | 4 / 4 PASS |
| harness_tool_contract | 9 / 9 PASS |
| history_session | 16 / 16 PASS |
| phase2_rereview_minor | 2 / 2 PASS |
| phase2_simplified_project_model | 10 / 10 PASS |
| phase2_static_review | 20 / 20 PASS |

### Frontend / package / diff

```text
npm exec --yes npm@10.8.2 -- ci
```

PASS。

```text
npm run check
```

`svelte-check found 0 errors and 0 warnings`。

```text
git diff --check
```

PASS。

说明：曾有一次把 `npm ci` 和 `npm run check` 并行执行，Windows 上 `npm ci` 删除 `node_modules` 时干扰了同时运行的 Svelte 检查；这是测试调度冲突。随后按 CI 实际顺序串行执行 `npm ci` → `npm run check` 均 PASS，没有据此修改产品代码。

## 独立 code review

对范围：

```text
a7ef61fa880354a1a754a8bfc0a24d70643ee2dd..f47f8d8e9a0b1ca9d055ee15aace88b7a2ebdc9c
```

执行项目内 `mcp-probe-kit code_review`。

结果：

- 没有发现 out-of-scope 文件；
- 没有提出具体语义代码缺陷；
- 正确识别到本轮包含 public contract / schema / version 变化；
- 要求提供 architecture validate/drift 证据。

该架构证据随后单独完成，不能把 code_review 的流程提醒误记成代码 PASS/FAIL 结论。

## ARC-8 架构校验

对最终 revision `f47f8d8`，按用户批准的设计、不变量、替代方案、真实 diff 与运行测试证据执行 `architecture mode=drift`。

最终结构化结果：

```json
{
  "passed": true,
  "gaps": [],
  "driftFindings": []
}
```

并确认：

- ARC-2 当前架构事实：completed；
- ARC-7 实施/测试验证：completed；
- ARC-8 drift：completed；
- `memoryCandidate.validated=true`，但本轮不额外写入跨项目长期 Memory。

## 保持不变的边界

- 一个 Connector + Workspace Pool 不变；
- Safe / Trusted / Dangerous 权限模式不变；
- Actions HTTP 不暴露 Active Project Session 工具；
- 不迁移、不覆盖旧 History 内容；
- 显式绝对路径只读参考仍可用；
- 无 host session 的 legacy 调用仍保留 `default_cwd` 兼容；
- `.5` RED 证据保持原样，作为 `.6` 整改来源。

## 真人实机验收状态

本轮可以进入 `.6` NSIS 候选构建，但当前仍不能写成 Phase 2 产品级 FINAL PASS。

真实 ChatGPT 验收至少需要验证：

1. 新对话明确 `F:\Nextcloud\project\逆向环境\utools` 后，先 `set_active_project`，再 `history_session_bootstrap`；
2. `get_active_project` 返回 uTools、`source=session`；
3. History 真实写入 `utools\docs\history-session`；
4. 同一聊天提到另一个项目完整路径时，不自动切换；
5. 明确说“切换当前项目到 ...”时才使用 `allow_rebind=true`；
6. 重启 Coding Tools MCP 后，原聊天仍恢复自己的 Session binding；
7. 第二个聊天可以独立绑定另一个项目，互不覆盖；
8. Connector 工具 schema 已刷新，客户端看到新的 `allow_rebind`，History schema 中不再有 `history_dir`。

由于 public MCP schema 已变化，安装 `.6` 后必须刷新 Connector 工具，并建议用**新建 ChatGPT 对话**执行正式验收。
