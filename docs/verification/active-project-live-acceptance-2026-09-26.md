# Active Project 多会话实机验收记录（2026-09-26）

## 结论

**FINAL: PASS**

已在真实 ChatGPT / Codex 多对话环境中验证：

> **同一个 Coding Tools MCP Connector + 同一个 Workspace Pool，可以让多个 Codex 对话同时、独立地绑定不同 Active Project，且互不覆盖。**

本结论不是仅靠单元测试或代码推断，而是经过真实 Connector、真实 ChatGPT 新会话、真实项目目录和交叉切换后的端到端实机验证。

## 验收对象

- 产品版本：`0.2.4-custom.3`
- 功能提交：`149c944588ccaf9476cef85e7c51a880a1501ad4`
- 分支：`custom/main`
- Workspace Pool：`F:\\Nextcloud\\project`
- Connector：单一 Coding Tools MCP Connector
- MCP 服务端工具数：`29`
- Active Project 工具：
  - `get_active_project`
  - `set_active_project`
  - `discover_projects`

对应 GitHub CI：

- Run：`36209242029`
- 结论：`success`
- Windows NSIS Artifact：`windows-nsis-149c944588ccaf9476cef85e7c51a880a1501ad4`
- Artifact SHA-256：`b990fdb63ce79540554802ff41ef3cb37ec7f98a04ec174b2e76aaae8fc671a0`

## 验收目标

```text
一个 Connector
    |
    +-- Codex 对话 A -> F:\Nextcloud\project\逆向环境\utools
    |
    +-- Codex 对话 B -> F:\Nextcloud\project\coding-tools-mcp
    |
    +-- 其他 Codex 对话 -> 其他项目
```

每个对话的 Active Project 必须由宿主会话隔离；一个对话切换项目后，不得覆盖其他对话的项目上下文。

## 前置发现：旧会话存在工具 Schema 缓存

升级到 `0.2.4-custom.3` 后，服务端 `server_info` 明确返回：

```text
tool_count = 29

get_active_project
set_active_project
discover_projects
```

但升级前已经打开的 ChatGPT 会话仍持有旧工具 Schema，直接尝试调用 `get_active_project` 时，客户端侧出现：

```text
TypeError:
tools.mcp__coding_tools_mcp__get_active_project is not a function
```

随后在 ChatGPT 插件设置中执行“刷新工具”，并新建对话后，新会话可以真实调用 `get_active_project`。

由此确认：

1. 服务端已正确注册新工具；
2. 旧 ChatGPT 会话不会因为服务端工具列表变化而热更新已注入工具表；
3. 刷新 Connector 工具后需要新建对话，新会话才会拿到最新工具 Schema。

## 实机验证过程

### Step 1：新建对话，确认新 Schema 已生效

发送：

```text
调 get_active_project，查看值
```

真实返回：

```text
Active Project: coding-tools-mcp
project_root: F:\Nextcloud\project\coding-tools-mcp
Workspace: F:\Nextcloud\project
session_scoped: true
source: default_cwd
```

判断：

- `get_active_project` 已能被真实调用；
- `session_scoped: true` 说明调用具备宿主会话上下文；
- `source: default_cwd` 说明此时尚未显式绑定 Session Active Project。

**Step 1: PASS**

### Step 2：对话 A 自动绑定 uTools 项目

用户发送：

```text
你能看到这个项目吗 F:\Nextcloud\project\逆向环境\utools
```

Codex 自动识别该目录为当前项目并读取项目根目录。随后 `get_active_project` 返回：

```text
Active Project: 逆向环境/utools
project_root: F:\Nextcloud\project\逆向环境\utools
Workspace: F:\Nextcloud\project
session_scoped: true
source: session
```

同时实际读到项目根内容，包括：

```text
README.md
UToolsConfigTool.sln
agents.md
app\
docs\
research\
tools\
utools_8.0.0-6\
```

关键证据是 `source: session`：这证明当前项目是当前宿主会话专属绑定，不是旧 `default_cwd` 偶然命中。

**Step 2: PASS**

### Step 3：对话 B 独立绑定 coding-tools-mcp

另开一个全新的 ChatGPT / Codex 对话，发送：

```text
你能看到这个项目吗 F:\Nextcloud\project\coding-tools-mcp
```

Codex 成功绑定并实际读取项目根目录，可看到：

```text
README.md
AGENTS.md
src\
src-tauri\
tests\
.github\
package.json
```

随后 `get_active_project` 确认：

```text
active_project: coding-tools-mcp
project_root: F:\Nextcloud\project\coding-tools-mcp
workspace: F:\Nextcloud\project
session_scoped: true
```

**Step 3: PASS**

### Step 4：回到对话 A 做交叉隔离验证

在对话 B 已经绑定 `coding-tools-mcp` 之后，重新回到原来的 uTools 对话 A，再次发送：

```text
调 get_active_project，查看值
```

真实返回仍然是：

```text
Active Project: 逆向环境/utools
project_root: F:\Nextcloud\project\逆向环境\utools
Workspace: F:\Nextcloud\project
session_scoped: true
source: session
```

判断：

- 对话 B 的绑定没有覆盖对话 A；
- 对话 A 仍保持自己的 `逆向环境/utools`；
- 真实宿主环境证明 `session -> active_project` 隔离成立。

这是本轮最关键的交叉验证。

**Step 4: PASS**

## 最终验收矩阵

| 验收项 | 实测结果 |
| --- | --- |
| 单一 Connector 连接 Workspace Pool | PASS |
| 新会话获取最新 29 工具 Schema | PASS |
| `get_active_project` 可真实调用 | PASS |
| 用户明确项目路径后自动绑定 | PASS |
| 项目不要求额外手工选择 | PASS |
| 对话 A `source=session` | PASS |
| 对话 B 独立绑定另一项目 | PASS |
| 对话 B 切换后，对话 A 不被覆盖 | PASS |
| 多对话同时独立操作不同项目 | PASS |

## 实机截图指纹

四张关键实机截图来自 ChatGPT 移动端真实会话，SHA-256 如下：

| 截图阶段 | SHA-256 |
| --- | --- |
| 新会话成功调用 `get_active_project`，返回 `source=default_cwd` | `6f54d0ab3d96e0e4e0140a1cbe1a01c2ab319ed5d9398f4d6eb5754c4b28341a` |
| uTools 对话自动绑定，返回 `source=session` | `9a02ea333702579183bfa10e397eba30647ae3f7053ffa77df3ecd8e87c51bad` |
| 第二个新对话绑定 `coding-tools-mcp` | `a77bf0b4a8367432a210f9da88ca7f8cfbdfe3233931ae793ff329e665f9d2d3` |
| 回到 uTools 对话交叉验证，仍返回 `source=session` | `84f117c8409b65914840adfa2e297a3fd989214c580e73e1627cb37098ae5149` |

这些哈希用于证明后续拿到的截图是否与本轮验收时的原始截图一致。截图本体保留在本次 ChatGPT 会话附件中，本仓库固化其内容摘要、验证过程和不可变指纹。

## 已证明的长期使用方式

```text
Coding Tools MCP Workspace Pool
F:\Nextcloud\project

ChatGPT / Codex
只配置一个 Connector
```

用户在任意新对话中直接说：

```text
review 项目 F:\Nextcloud\project\coding-tools-mcp
```

或：

```text
请查看这个项目 F:\Nextcloud\project\逆向环境\utools
```

AI 会自然把该目录作为当前对话 Active Project，后续文件、Exec、Git、Patch 等相对操作以该项目为默认项目上下文。

## 已知边界

本轮冻结的是 **Active Project MVP**。

已经 Project-aware：

- 文件读取 / 搜索
- Exec / 相对 workdir
- Git
- Patch
- Active Project 根目录灾难性删除保护

尚未下沉到 Project identity：

- History Session
- Harness / Task identity

因此当前可以证明“代码操作上下文按对话和项目隔离”，但 History / Harness 仍以 Workspace Pool 为身份边界。后续第二阶段如继续实施，应单独验收，不应修改本记录的 PASS 口径。

## 证据等级说明

本记录基于三层证据共同形成：

1. **代码级**：`149c944` 已实现 Session-scoped Active Project；
2. **CI 级**：GitHub Actions Run `36209242029` 全绿，Windows 原生回归与 NSIS 构建通过；
3. **真实宿主级**：真实 ChatGPT / Codex 中多个新对话分别绑定不同本机项目，并完成交叉回查，确认项目绑定未被覆盖。

因此本轮结论不是根据实现推断，而是经过真实用户路径的端到端交叉验证。

## 冻结结论

**Active Project MVP：PASS，可冻结。**

> **Codex 永远只配置一个 Connector，但可以同时、独立地操作任意多个项目。**
