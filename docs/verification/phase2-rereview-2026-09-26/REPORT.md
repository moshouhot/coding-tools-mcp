# Phase 2 第二轮静态复审与隔离复现

## 结论：HOLD，等待功能整改审批

审查基线：`3adfb99f9f7fbc0ad029d7c835daa28da416441a`，源码版本 `0.2.4-custom.5`。

本轮修正了一个小型返回值问题和文档错误，但确认仍有 **6 个行为缺陷，归为 3 组功能整改**。这些问题尚未修复，也未被隐藏为已通过的验收。暂不建议把 `.5` 当作正式实机验收候选。

上一轮“350 项测试通过”是历史测试结果，不等于所有恢复和隔离边界都正确。此前“可以进入验收”的判断覆盖不足，本轮以新增反例收窄该结论。原报告、原日志不覆盖、不伪装成已经通过本轮复审。

当前 Connector 实际返回的运行版本仍是 `.4`。本轮行为证据来自本机编译 `.5` 源码后的临时目录测试，不冒充已经安装 `.5` 的 ChatGPT 实机验证。未安装、重启或替换用户正在使用的服务。

## 审查边界与设计目标

重点审查上一轮整改 diff（`afa476b..3adfb99`）及关联的 context、dispatcher、History storage、Harness、MCP/Actions 构造与测试。不是对全部第三方依赖和所有业务模块作“绝无漏洞”保证。

保留一个 Connector + Workspace Pool；明确指定的项目自动绑定当前会话，其他会话互不干扰；不因顺带提及别处目录切换项目；History/Harness 跟随项目；standalone 不强制 Task；不削弱权限策略，不自动迁移历史数据。

## 需要审批的功能整改

### A. 项目绑定持久化仍会丢失恢复状态或覆盖他人记录

**R1 / P1：损坏记录修复后，再重启一次，旧会话静默回落。**

证据位置：`src-tauri/src/tools/context.rs:112-114, 198-229, 241-254`。

复现顺序：旧会话绑定 Alpha；临时状态文件被破坏；重新创建上下文时能够正确报错；另一个会话绑定 Beta；再次创建上下文，把默认目录设为 Beta；查询旧会话。

实际返回 `ok=true, source=default_cwd, active_project=beta`。损坏标志只存在内存，重绑另一个会话写出有效 JSON 后，下一次启动只看到一份缺少旧会话的正常文件。旧回归只检查第二次重启之前，没有覆盖此路径。

**R2 / P1（条件性）：两个上下文共享同一持久化文件，顺序写入也会丢记录。**

证据位置：`context.rs:108-114, 241-254, 359-384`；关联生产构造为 `src-tauri/src/actions/listener.rs:141`、`src-tauri/src/mcp/server.rs:200`。

复现：两个 ToolContext 先分别载入同一个空状态；A 写 s1；B 写 s2；第三个上下文重新读取，只剩 s2。原因是只锁各自内存并整表覆盖，没有对共享持久化状态做读改写事务。

**限制：这不等于同一个共享 ToolContext 内的两个普通聊天必然串项目。** 多服务、重复实例或不同 profile 共用同一路径才涉及这个多写者边界。自动化验证了双上下文顺序写入，没有冒充已在用户日常单实例配置中发生事故。

建议审批：持久化“状态损坏/需要重绑”的恢复标志，不让第二次启动忘记它；保留损坏证据；显式重绑只恢复对应会话。对同一 store 使用统一锁和读取最新状态后的原子更新，避免旧内存整表覆盖；写入失败不宣告成功。不要求引入数据库或新建多套 Connector。

### B. History 的“不参与代码基线检查”规则缺少目录归属边界

**R3 / P1：一次只读搜索就能把真正的源代码目录列为忽略项。**

证据位置：`src-tauri/src/tools/history/mod.rs:352-355, 744-765`，`src-tauri/src/tools/context.rs:281-315`，`src-tauri/src/tools/history/storage.rs:34-68`。

复现：Alpha 有 `src/code.rs`；执行 `history_session_search(history_dir="src")`，里面没有有效历史档案；创建 Harness Task；从工具之外修改 `src/code.rs`；再执行 `pwd`。

实际执行成功，未报 `FILE_CHANGED_EXTERNALLY`。公共的 scope 解析无条件把调用者给出的目录登记成 managed path，发生在确认历史归属之前。搜索没有修改业务代码，但它改变了以后检查业务代码的规则。这不是 Workspace Pool 的访问隔离问题，而是代码基线保护被错误地豁免。

**R4 / P2：标称只读的 History 搜索依赖写入 managed-path 状态。**

测试在临时 Harness 目录放置同名普通文件，阻止创建 `managed-project-paths` 目录。搜索随后返回 `MANAGED_PROJECT_PATH_STORE_UNAVAILABLE`，错误为 Windows os error 183。这里实测的是元数据路径不可用，不是 ACL 拒绝，也没有证据表明是 rename 冲突。源码中无条件持久化调用解释了这个读写耦合。

**R5 / P2：项目正好就是 Workspace 根目录时，自定义 History 排除规则失效。**

证据位置：`context.rs:255-278`，`src-tauri/src/tools/dispatch.rs:114-131`。

复现：绑定 `.`，使用 `.ai/session-history`；bootstrap → start_task → checkpoint → `pwd`。实际仍报 `FILE_CHANGED_EXTERNALLY`，因为根目录分支提前返回初始 Harness 的 clone，没有带入后来登记的自定义目录。

建议审批：把“解析/读取历史”与“登记自维护文件”分离。read/search/不修复的 validate 不更改排除规则。只允许经过归属确认的专用档案目录登记，拒绝整个项目根、业务源码目录及混杂业务文件的目录；自定义目录的兼容和迁移规则需要明确。子项目、Workspace 根和 fallback 采用一致的排除规则。不得用“历史写完后重拍整个代码基线”掩盖真实外部代码改动。

### C. 已绑定目录被换成指向兄弟项目的链接时，仍会悄悄跟过去

**R6 / P1：池内目录链接重定向没有被识别为绑定目标变化。**

证据位置：`src-tauri/src/tools/context.rs:321-329`。

复现：绑定临时 Alpha；移除临时 Alpha 目录；创建 Alpha → Beta 的目录符号链接；在原会话读取 `name.txt`。

实际返回 `ok=true, path=beta/name.txt, content=beta`。校验只检查新解析目标仍在 Workspace Pool 内，没有核对它是否还是最初绑定的规范目标。本机符号链接创建成功，此测试没有走 `UNTESTED` 早退。

建议审批：后续访问若解析后的规范目标与已绑定目标不同，明确返回项目不可用/目标变化，由用户明确指定后再重绑。原本就合法使用的链接可在首次绑定时解析并记录目标，不必禁止所有链接。

限制：此反例要求先发生本地文件系统修改，不是凭远端一个普通读取请求就能重定向。也不宣称能够识别“同一路径被删后重新建成不同普通目录”的文件系统身份变化；那需要另外的身份设计与测试。

## 本轮已直接完成的小修

**M1：权限查询回显夹带内部字段。** `dispatch.rs:211,229` 两处复用既有 `strip_internal_context`，只清除顶层 `_host_session_key` 与 `_active_project_root`，保留用户嵌套参数，保持 granted/unsupported、权限判断和路由不变。

新增 `src-tauri/tests/phase2_rereview_minor.rs` 两条正常回归：修复前 2 failed（exit 101）；修复后 2 passed（exit 0）。正式产品代码差异仅这两处返回值清理。

**M2：文档初始化顺序与工程状态。** 中英文 README 示例改成“明确指定项目时先 set_active_project，再 bootstrap”，说明 project_id 与切换项目后的初始化。项目上下文文档不再声称“0.0.0、骨架未创建、17 个工具”；版本以清单为准，core 工具面以 registry 为准。

**M3：前轮证据校验补正，不改写冻结证据。** `previous-manifest-verification.json` 确认旧 manifest 的两个日志各少一个末尾 LF；给当前日志末尾加回一个 LF 后 SHA256 恰好恢复旧 manifest 值。其余文件及指定 Git 源码 blob 匹配。原文件与原 manifest 保留；本轮补充实际哈希和原因，不能再声称原 manifest 全部匹配。

| 旧日志 | 当前字节数 | 当前 SHA256 |
| --- | ---: | --- |
| full-regression.log | 29210 | bf3518b1d319fb529906effeb9171ebcaea5b32dc374c1da3bb52b9b32773793 |
| minor-fixed.log | 2368 | 11646ddb1db3302a8270b7c1d991efad086b5a4aa1607577665ff56ff32cd75b |

## 验证结果的正确读法

| 验证 | 结果 | 证据目录 |
| --- | --- | --- |
| 小修前的返回值测试 | 2 failed，exit 101 | checks/minor-before |
| 小修后的返回值测试 | 2 passed，exit 0 | checks/minor-after |
| 正常 Rust 回归 | 352 passed / 0 failed / 0 ignored，exit 0 | checks/rust-final |
| 前端检查 | 0 errors / 0 warnings，exit 0 | checks/frontend-final |
| 主审独立运行新增反例（原 .5 编译产物） | 6 个安全期望失败，另 1 个“确认错误行为”的测试通过，exit 101 | checks/independent-red-probes |
| 两行小修后重新编译并重跑反例 | 同样 6 failed + 1 错误行为确认，exit 101 | checks/red-after-minor |

**正常回归绿，不抵消六个新增反例红。** 额外通过的一个用例刻意断言错误 fallback 行为，仅用于确认缺陷，绝不是验收通过。正常回归包含名称为 real_history_fixture 的可选测试；不据其通过推断真人交互已测试。

失败探针源码保存于 `repro/probes/phase2_rereview_probe.rs`。重放方式：`python docs/verification/phase2-rereview-2026-09-26/repro/run_probe.py --label NEW_LABEL`。脚本仅临时创建专用集成测试，保留 stdout/stderr、生产 diff、源文件哈希和真实退出码，再核对内容后移除自己的临时测试。失败用例没有以 `#[ignore]` 冒充正常回归通过；未审批的缺陷用独立红色复现包保留。

## 独立复核、工具限制与证据整理

主审直接阅读相关源码并独立执行反例。第一轮 Pi 辅助模型完成探针构建和红色日志，但因定位日志时的文件搜索发生 idle timeout，没有交付完整报告。`advisor/RUN_RESULT.json` 原样保留 TIMEOUT，不计成功。其 runner 的根目录计算错误把日志放到证据目录的嵌套目录；经逐字节比对后归拢到 `repro/logs`，搬移映射在 `advisor/evidence-location-correction.json`，原 runner 保留为 `.py.txt` 证据。

第二次限时只读 advisor 已完成，退出码 0，报告与结构化结果位于 `final-advisor/`。它认可小修范围并支持 HOLD，但没有自己运行测试、没有在该次任务中阅读 History 源码；这两项由主审直接阅读与实测补足。它的“修复后测试尚无证据”是其阅读时点限制，不能覆盖本轮后续已保存的实测结果。其 os error 183 的一般性描述，以本报告 R4 的具体 fixture 与源码解释为准。

图谱第一次查询因索引陈旧而内部返回 Symbol not found，未把外层 ok 当成影响面为零。用 managed GitNexus `--index-only --workers 1` 刷新索引后，确认统一 dispatcher 的直接调用者为 call_tool 与 call_tool_with_audit；图谱影响风险 HIGH，已告知用户并跑全量回归。提交前 detect-changes 的已追踪生产符号只命中 call_tool_prepared，其余为本轮文档标题；未追踪的新测试和证据另外用 Git 暂存清单检查。

## 交付与验收状态

修改保留在本地 `audit/phase2-rereview-20260926`，只提交小修、测试和审查证据；不推送、不合并到 custom/main，不制作或安装新候选包。版本号不提升。

小修提交：`3d3134f`（`fix-rereview-permission-echo-and-docs`）。测试日志里的 HEAD 是测试时的基线，生产文件哈希及 `red-after-minor/production.diff` 记录了当时未提交的两行小修；不把基线 SHA 冒充已经包含这两行。

当前不是要求用户安装测试的时点。先审批上述 A/B/C 三组功能边界，再实施、复审并把这些反例转为正式通过的回归，随后才进入真实 ChatGPT 的安装、刷新工具、跨会话与重启验收。
