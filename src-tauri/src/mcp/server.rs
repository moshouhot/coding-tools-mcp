use std::sync::Arc;

use serde_json::Value;

use crate::audit::{AuditRequestContext, AuditStore};
use crate::mcp::upstream::UpstreamMcpManager;
use crate::tools::{
    call_tool_with_audit, list_tools_for_profile, record_tool_rejection_with_audit,
    wrap_mcp_tool_result, SharedToolContext, ToolContext, Workspace,
};
use crate::workspace::AuthConfig;

pub struct McpState {
    pub tools: SharedToolContext,
    pub upstream: Arc<UpstreamMcpManager>,
}

impl McpState {
    pub fn audit_store(&self) -> Option<AuditStore> {
        self.tools.audit_store()
    }
}

pub type SharedState = Arc<McpState>;

// 生产入口接收监听层采集的 headers 上下文；原 handle_request 仅为无 HTTP 环境的测试生成
// 最小上下文。协议分发保持与 Axum 解耦，且只让 tools/call 进入工具审计。
#[cfg(test)]
pub fn handle_request(state: &SharedState, body: &Value) -> Value {
    let request = AuditRequestContext {
        transport: "mcp".into(),
        method: body
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string),
        request_id: body.get("id").and_then(crate::audit::request_id_from_value),
        route: Some("/mcp".into()),
        ..AuditRequestContext::default()
    };
    handle_request_with_context(state, body, &request)
}

pub fn handle_request_with_context(
    state: &SharedState,
    body: &Value,
    request: &AuditRequestContext,
) -> Value {
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    if id.is_null() && method.starts_with("notifications/") {
        return Value::Null;
    }

    let result = match method {
        "initialize" => Ok(initialize_result()),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => {
            let mut tools = list_tools_for_profile(&state.tools.tool_profile);
            tools.extend(state.upstream.public_tools().iter().cloned());
            Ok(serde_json::json!({ "tools": tools }))
        }
        "tools/call" => handle_tools_call(state, &params, request),
        _ => Err(serde_json::json!({
            "code": -32601,
            "message": format!("Method not found: {method}")
        })),
    };

    match result {
        Ok(result) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    }
}

fn initialize_result() -> Value {
    serde_json::json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {
            "tools": { "listChanged": false },
            "logging": {}
        },
        "serverInfo": {
            "name": "coding-tools-mcp",
            "title": "Coding Tools MCP",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Use these tools only for local coding operations inside the configured workspace. The configured workspace may be a pool containing many independent projects. When the user's request explicitly identifies a directory inside the workspace as the project, repository, repo, codebase, work tree, or clearly makes that directory the primary coding target, automatically call set_active_project with that directory before any project-scoped initialization or project work. The user's explicit directory designation is authoritative; do not require .git, package.json, Cargo.toml, or other project markers and do not ask for confirmation. Reuse the active project for later requests in the same conversation. Do not switch active project merely because the user incidentally references another file or directory. If the user names a project without a path, use discover_projects and auto-bind only when the match is unique. Active Project is scoped to the host conversation and persisted locally so other conversations may work in other projects through the same connector. If a previously bound project becomes unavailable, do not silently fall back to another project; rebind explicitly. History and Harness are project-aware when a Session Active Project exists; otherwise they retain the workspace-level fallback. At the start of every new ChatGPT conversation, before answering the user's first request, initialize project context first when the first request identifies a project, then call history_session_bootstrap exactly once and pass the user's verbatim first request as initial_user_input. Treat bootstrap as required conversation initialization: it creates or resumes a lossless Markdown archive under the active project when one is bound, and returns bounded current state, not all history. Use history_session_search followed by history_session_read only when exact earlier context is needed. history_session_read returns a bounded UTF-8-safe page; follow next_cursor with the returned content hash until the relevant archive is complete. Repeated successful bootstrap calls in the same project resume the same session and must not create duplicates. Preserve session_key, current_path, and project_id returned by bootstrap, then pass them unchanged as session_key, expected_path, and project_id to every history_session_checkpoint call. When this conversation switches Active Project, bootstrap/resume History for that project before checkpointing it. After completing each user-requested task in the conversation, call history_session_checkpoint before the final response and pass that user's verbatim request as raw_user_input. Only state that progress was saved after checkpoint returns ok=true with the same session_key, project_id, and path. The server cannot access ChatGPT transcript text that was not provided as a tool argument; persistence is not automatic background persistence."
    })
}

fn handle_tools_call(
    state: &SharedState,
    params: &Value,
    request: &AuditRequestContext,
) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| serde_json::json!({ "code": -32602, "message": "Missing tool name" }))?;
    if state.upstream.owns_tool(name) {
        let result =
            tauri::async_runtime::block_on(state.upstream.call_tool(name, raw_tool_arguments(params)));
        return Ok(normalize_upstream_result(result));
    }

    let args = tool_arguments(name, params);

    let canonical_name = crate::tools::registry::canonical_tool_name(name);
    let known = crate::tools::registry::exposed_tool_names(&state.tools.tool_profile);
    // 未知/未暴露工具在 dispatcher 前提前返回，拒绝记录必须放在这里；通过校验的路径改用
    // audited wrapper，审计成功与否都不改变原 MCP 结果。
    if !known.iter().any(|n| n == &canonical_name) {
        let message = format!("Unknown tool: {name}");
        record_tool_rejection_with_audit(
            state.tools.as_ref(),
            request,
            name,
            &args,
            "UNKNOWN_TOOL",
            &message,
        );
        return Err(serde_json::json!({
            "code": -32602,
            "message": message,
            "data": { "reason": "unknown_tool" }
        }));
    }

    let structured = call_tool_with_audit(state.tools.as_ref(), canonical_name, &args, request);
    Ok(wrap_mcp_tool_result(canonical_name, &args, structured))
}

fn normalize_upstream_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(result) if result.get("content").is_some() => result,
        Ok(result) => serde_json::json!({
            "content": [{ "type": "text", "text": result.to_string() }],
            "structuredContent": result,
            "isError": false
        }),
        Err(message) => serde_json::json!({
            "content": [{ "type": "text", "text": message }],
            "structuredContent": {
                "ok": false,
                "status": "error",
                "error": {
                    "category": "upstream_mcp",
                    "message": "本地 MCP 工具调用失败"
                }
            },
            "isError": true
        }),
    }
}

fn tool_arguments(name: &str, params: &Value) -> Value {
    let mut args = raw_tool_arguments(params);
    let _ = name;
    if let Some(object) = args.as_object_mut() {
        object.remove("_host_session_key");
        object.remove("_active_project_root");
    }
    if let Some(session_key) = params
        .get("_meta")
        .and_then(|meta| meta.get("openai/session"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !args.is_object() {
            args = serde_json::json!({});
        }
        args["_host_session_key"] = Value::String(session_key.to_string());
    }
    args
}

fn raw_tool_arguments(params: &Value) -> Value {
    params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

// 审计在 SharedState 创建时绑定：此处同时拥有正式 workspace_id，且尚未被多请求共享；
// 测试构造路径可不调用 with_audit，因此不会写入用户数据库。
pub fn new_state(
    workspace: Workspace,
    workspace_id: String,
    auth: AuthConfig,
    policy: crate::tools::policy::PolicySettings,
    tool_profile: String,
    permission_mode: String,
    upstream: Arc<UpstreamMcpManager>,
) -> SharedState {
    Arc::new(McpState {
        tools: Arc::new(
            ToolContext::from_workspace(workspace, auth, policy, tool_profile, permission_mode)
                .with_audit(workspace_id),
        ),
        upstream,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use crate::mcp::upstream::UpstreamMcpManager;
    use crate::tools::ToolContext;

    use super::{handle_request, initialize_result, raw_tool_arguments, tool_arguments, McpState};

    #[test]
    fn initialize_instructions_define_the_history_persistence_workflow() {
        let initialized = initialize_result();
        let instructions = initialized["instructions"].as_str().expect("instructions");
        assert!(instructions.contains("history_session_bootstrap"));
        assert!(instructions.contains("At the start of every new ChatGPT conversation"));
        assert!(instructions.contains("before answering the user's first request"));
        assert!(instructions.contains("required conversation initialization"));
        assert!(instructions.contains("initial_user_input"));
        assert!(instructions.contains("must not create duplicates"));
        assert!(instructions.contains("history_session_checkpoint"));
        assert!(instructions.contains("set_active_project"));
        assert!(instructions.contains("discover_projects"));
        assert!(instructions.contains("Active Project is scoped to the host conversation"));
        assert!(instructions.contains("History and Harness are project-aware"));
        assert!(instructions.contains("do not ask for confirmation"));
        assert!(instructions.contains("raw_user_input"));
        assert!(instructions.contains("history_session_search"));
        assert!(instructions.contains("history_session_read"));
        assert!(instructions.contains("follow next_cursor"));
        assert!(instructions.contains("session_key, current_path, and project_id returned by bootstrap"));
        assert!(instructions.contains("session_key, expected_path, and project_id"));
        assert!(instructions.contains("After completing each user-requested task"));
        assert!(instructions.contains("before the final response"));
        assert!(instructions.contains("checkpoint returns ok=true"));
        assert!(instructions.contains("not automatic background persistence"));
        let bind_position = instructions
            .find("initialize project context first")
            .expect("project binding order");
        let bootstrap_position = instructions
            .find("then call history_session_bootstrap")
            .expect("bootstrap order");
        assert!(bind_position < bootstrap_position);
    }

    #[test]
    fn initialize_does_not_claim_tool_catalog_notifications_without_a_stream() {
        let initialized = initialize_result();

        assert_eq!(initialized["capabilities"]["tools"]["listChanged"], false);
    }

    #[test]
    fn workspace_prompt_initializes_or_restores_a_chatgpt_session() {
        let component = include_str!("../../../src/lib/components/ChatGptSessionPrompt.svelte");

        assert!(component.contains("ChatGPT 新会话启动提示词"));
        assert!(component.contains("请初始化或恢复当前项目会话"));
        assert!(component.contains("如果没有历史记录"));
        assert!(component.contains("initial_user_input"));
        assert!(component.contains("raw_user_input"));
        assert!(component.contains("history_session_search"));
        assert!(component.contains("history_session_checkpoint"));
        assert!(component.contains("后续相对路径、History 和 Harness 都继续使用当前项目"));
        let bind_position = component.find("set_active_project").expect("set active project");
        let bootstrap_position = component
            .find("history_session_bootstrap")
            .expect("history bootstrap");
        assert!(bind_position < bootstrap_position);
        assert!(!component.contains("打开连接器设置"));
    }

    #[test]
    fn chatgpt_session_metadata_is_injected_for_all_local_tools() {
        let params = json!({
            "arguments": {"session_key": "explicit"},
            "_meta": {"openai/session": "chatgpt-conversation"}
        });
        let history = tool_arguments("history_session_bootstrap", &params);
        assert_eq!(history["session_key"], "explicit");
        assert_eq!(history["_host_session_key"], "chatgpt-conversation");

        let existing = tool_arguments("read_file", &params);
        assert_eq!(existing["session_key"], "explicit");
        assert_eq!(existing["_host_session_key"], "chatgpt-conversation");

        let upstream_safe = raw_tool_arguments(&params);
        assert_eq!(upstream_safe["session_key"], "explicit");
        assert!(upstream_safe.get("_host_session_key").is_none());
    }

    #[test]
    fn local_tool_arguments_cannot_spoof_internal_session_context() {
        let params = json!({
            "arguments": {
                "path": "project-a",
                "_host_session_key": "spoofed",
                "_active_project_root": "other-project"
            }
        });
        let args = tool_arguments("set_active_project", &params);
        assert!(args.get("_host_session_key").is_none());
        assert!(args.get("_active_project_root").is_none());

        let params = json!({
            "arguments": {"path": "project-a", "_host_session_key": "spoofed"},
            "_meta": {"openai/session": "trusted-host-session"}
        });
        let args = tool_arguments("set_active_project", &params);
        assert_eq!(args["_host_session_key"], "trusted-host-session");
    }

    #[test]
    fn host_session_key_takes_precedence_over_explicit_session_key() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(McpState {
            tools: Arc::new(
                ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                    .expect("tool context"),
            ),
            upstream: Arc::new(UpstreamMcpManager::empty()),
        });
        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "history_session_bootstrap",
                    "arguments": {
                        "session_key": "explicit-session",
                        "initial_user_input": "保存首轮原文"
                    },
                    "_meta": {"openai/session": "chatgpt-session"}
                }
            }),
        );
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["session_key_source"], "platform_conversation_id");
        assert_eq!(structured["session_key"], "chatgpt-session");
        assert_eq!(structured["initial_input_captured"], true);
        let content = fs::read_to_string(workspace.path().join("docs/history-session/1.md"))
            .expect("read history file");
        assert!(content.contains("**Session key:** chatgpt-session"));
        assert!(!content.contains("**Session key:** explicit-session"));
    }

    #[test]
    fn active_project_is_bound_from_openai_session_metadata() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        fs::create_dir_all(workspace.path().join("project-a")).expect("project-a");
        fs::create_dir_all(workspace.path().join("project-b")).expect("project-b");
        fs::write(workspace.path().join("project-a/name.txt"), "A\n").expect("write A");
        fs::write(workspace.path().join("project-b/name.txt"), "B\n").expect("write B");
        let state = Arc::new(McpState {
            tools: Arc::new(
                ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                    .expect("tool context"),
            ),
            upstream: Arc::new(UpstreamMcpManager::empty()),
        });

        for (session, project) in [("chat-a", "project-a"), ("chat-b", "project-b")] {
            let response = handle_request(
                &state,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "set_active_project",
                        "arguments": {"path": project},
                        "_meta": {"openai/session": session}
                    }
                }),
            );
            assert_eq!(response["result"]["structuredContent"]["ok"], true);
            assert_eq!(
                response["result"]["structuredContent"]["active_project"],
                project
            );
        }

        for (session, expected) in [("chat-a", "A\n"), ("chat-b", "B\n")] {
            let response = handle_request(
                &state,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "read_file",
                        "arguments": {"path": "name.txt"},
                        "_meta": {"openai/session": session}
                    }
                }),
            );
            assert_eq!(response["result"]["structuredContent"]["content"], expected);
        }
    }

    #[test]
    fn legacy_grep_calls_are_mapped_to_the_public_grep_text_tool() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        fs::write(workspace.path().join("sample.txt"), "catalog needle")
            .expect("write sample file");
        let state = Arc::new(McpState {
            tools: Arc::new(
                ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                    .expect("tool context"),
            ),
            upstream: Arc::new(UpstreamMcpManager::empty()),
        });

        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "grep",
                    "arguments": {"query": "needle", "path": "."}
                }
            }),
        );

        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["structuredContent"]["ok"], true);
    }
}
