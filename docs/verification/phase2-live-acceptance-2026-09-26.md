# Phase 2 真人 ChatGPT 实机验收铁证（2026-09-26）

## 最终结论

**PHASE 2 CORE LIVE ACCEPTANCE: PASS**

本次验收对象为 `0.2.4-custom.6` 的“一对话一项目”简化模型。验收使用真实 ChatGPT 对话、真实已安装 Coding Tools MCP、真实 Workspace Pool 与真实项目目录，不是临时单元测试或 mock。

验收范围按用户最终批准的产品原则收敛为：

> 一个 ChatGPT / Codex 对话默认只对应一个项目；不同对话可通过同一个 Connector 独立绑定不同项目；提到另一个项目完整路径不会自动切换，完整绝对路径可用于只读参考。

本文件只记录真人实机已经确认的核心链路。损坏状态恢复、symlink 目标偷换、跨项目 Exec、History 自定义目录等边界由自动化永久回归覆盖，不在本次人工验收中重复折腾。

## 验收版本与仓库状态

- 运行中 MCP：`coding-tools-mcp 0.2.4-custom.6`
- Workspace Pool：`F:\Nextcloud\project`
- 正式分支：`custom/main`
- 正式 HEAD：`9129ef7bb9114097b89ea0feff220f75203e899d`
- Git：clean
- upstream：`origin/custom/main`
- ahead / behind：`0 / 0`
- core tools：29

当前 live `server_info` 已实际返回：

```text
version = 0.2.4-custom.6
workspace = F:\Nextcloud\project
tool_profile = core
tool_count = 29
```

## GitHub CI / 安装候选证据

- GitHub Actions Run：`36231159589`
- head SHA：`9129ef7bb9114097b89ea0feff220f75203e899d`
- conclusion：`success`

第二轮最终 CI 已确认：

```text
Frontend                                  SUCCESS
Linux Rust build/test                     SUCCESS
Windows frontend + Rust/native regression SUCCESS
Windows NSIS build                        SUCCESS
Installer artifact upload                 SUCCESS
```

第一次 `.6` CI 曾在 Linux 的目录 symlink 测试清理代码处失败：测试使用了 Windows 的 `remove_dir` 语义。产品逻辑没有失败；测试随后改为 Windows/Linux 各自正确的 symlink 删除 API，第二轮 CI 全绿。该修复只修改测试代码。

最终安装 artifact：

```text
name   = windows-nsis-9129ef7bb9114097b89ea0feff220f75203e899d
id     = 10901784138
size   = 6312720 bytes
sha256 = f902a8366b3e6f81c3fb3a60c08a2052635414e16d9f36de0d518b8eb5ba0a8c
```

## 真人验收步骤与结果

### A 对话：绑定 uTools

用户在新对话 A 中发送：

```text
请查看这个项目 F:\Nextcloud\project\逆向环境\utools
```

随后调用 `get_active_project`。

实机返回确认：

```text
active_project   = 逆向环境/utools
project_root     = F:\Nextcloud\project\逆向环境\utools
source           = session
requires_binding = false
```

这证明对话 A 已真实绑定 uTools，而不是使用 `default_cwd`。

### B 对话：独立绑定 coding-tools-mcp

用户另开新对话 B，查看：

```text
F:\Nextcloud\project\coding-tools-mcp
```

随后调用 `get_active_project`。

实机返回确认：

```text
active_project   = coding-tools-mcp
project_root     = F:\Nextcloud\project\coding-tools-mcp
source           = session
requires_binding = false
```

### 再回 A：B 没有覆盖 A

在 B 已完成绑定后，用户重新回到对话 A 再次调用 `get_active_project`。

实机仍返回：

```text
active_project   = 逆向环境/utools
project_root     = F:\Nextcloud\project\逆向环境\utools
source           = session
requires_binding = false
```

因此确认：

```text
同一个 Connector
同一个 Workspace Pool

对话 A -> uTools
对话 B -> coding-tools-mcp

B 的绑定不会覆盖 A
A 返回后仍保持原项目
```

该结果与第一阶段 Active Project MVP 的交叉隔离铁证一致，并证明 `.6` 的新持久化模型没有破坏这一核心能力。

## 跨项目完整路径只读：不会自动切项目

在已经绑定 uTools 的对话 A 中，用户发送：

```text
看一下 F:\Nextcloud\project\coding-tools-mcp\README.md，不要切换当前项目
```

ChatGPT 使用显式绝对路径成功读取了另一个项目的 README。

随后再次调用：

```text
get_active_project
```

真实返回仍为：

```text
active_project   = 逆向环境/utools
project_root     = F:\Nextcloud\project\逆向环境\utools
source           = session
requires_binding = false
```

因此真人确认 `.6` 的关键新规则：

> **提到或只读访问另一个项目的完整绝对路径，不会自动改变当前对话的 Active Project。**

这正好符合用户最终使用习惯：一个对话基本只讨论一个项目；偶尔需要参考别的项目时会给出完整路径。

## 额外 live fail-closed 观察

在本验收证据整理对话中，升级 `.6` 后当前对话尚未建立 Session Active Project binding 时，尝试进行 Git 项目操作，服务器真实返回：

```text
ACTIVE_PROJECT_REQUIRED
This conversation has no Active Project binding;
refusing to fall back to default_cwd.
```

随后明确调用：

```text
set_active_project(F:\Nextcloud\project\coding-tools-mcp)
```

成功建立当前验收对话自己的 binding 后，Git / server_info 才继续正常执行。

这不是截图人工步骤的一部分，但它提供了额外 live 证据：`.6` 已真实执行“host Session 无 binding 不允许 silent default_cwd fallback”的规则。

## 有效截图指纹

本轮真人验收只采纳以下三张截图为核心证据。截图本体来自本次 ChatGPT 对话；仓库记录 SHA-256 指纹用于日后核对原始图片是否一致。

### 1. 对话 A：uTools 绑定，并在 B 测试后返回 A 仍保持 uTools

```text
source image: 1000028851.jpg
sha256: 5c03a6242ac0e09b0d206f9e4b4dd2dcf06f7db13ffe84d5c931e68d7fafb481
```

### 2. 对话 B：独立绑定 coding-tools-mcp

```text
source image: 1000028850.jpg
sha256: 4c4209e66c48996983944d43ce6829616ba7bae7aaa225d7f10ea7ec2c61c5d9
```

### 3. 对话 A：绝对路径只读 coding-tools-mcp README 后仍保持 uTools

```text
source image: 1000028856.jpg
sha256: 08a46542f83490f6bbd9d17d974fb942906fb704f475081dc0ec4fc59b0e719b
```

曾有一张在 B 对话中执行“不要切换项目”的截图，因为读取目标本身就是 B 当前绑定的 coding-tools-mcp，无法证明跨项目只读，因此明确**不计入本轮验收证据**。

## 与自动化证据的关系

`.6` 在真人验收之前已经完成：

- Rust 全量：`362 passed / 0 failed / 0 ignored`
- `phase2_simplified_project_model`：`10 / 10 PASS`
- `phase2_static_review`：`20 / 20 PASS`
- `call_tool_contract`：`31 / 31 PASS`
- `history_session`：`16 / 16 PASS`
- Frontend：`0 errors / 0 warnings`
- `npm ci`：PASS
- `git diff --check`：PASS
- independent `code_review`：未发现具体语义 blocker
- ARC-8 final drift：`passed=true`, `gaps=[]`, `driftFindings=[]`
- GitHub Linux / Windows / NSIS：最终 SUCCESS

自动化负责边界场景；本文件的人工作用是确认真实 ChatGPT Host Session / Connector / Workspace Pool 链路确实与设计一致。

## 最终冻结结论

基于 `.6` 自动化、静态审查、跨平台 CI、NSIS 构建及本轮真人 ChatGPT A/B 交叉验证：

> **Phase 2 核心实机验收正式 PASS。**

可以冻结以下产品行为：

1. 一个 Connector 服务 Workspace Pool 中多个项目；
2. 一个对话默认稳定绑定一个项目；
3. 不同对话可以独立绑定不同项目，互不覆盖；
4. host Session 的项目 binding 来源为 `session`，不依赖 `default_cwd`；
5. 显式绝对路径可以只读参考另一个项目，不会自动切换当前项目；
6. 真正跨项目执行或明确切换当前项目，需要显式 rebind；
7. `.5` RED 证据和 `.6` 自动化整改证据继续保留，形成完整问题发现 -> 整改 -> 真人验收证据链。
