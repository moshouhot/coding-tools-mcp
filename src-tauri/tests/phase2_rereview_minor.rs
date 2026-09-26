use std::fs;

use coding_tools_mcp_desktop_lib::tools::{call_tool, ToolContext};
use serde_json::json;

fn assert_public_permission_echo(mode: &str) {
    let temp = tempfile::tempdir().expect("isolated fixture");
    let pool = temp.path().join("pool");
    fs::create_dir_all(pool.join("alpha")).unwrap();
    let mut ctx = ToolContext::for_test(pool, temp.path().join("harness")).unwrap();
    ctx.permission_mode = mode.into();
    ctx.policy.permission_mode = mode.into();
    let selected = call_tool(
        &ctx,
        "set_active_project",
        &json!({"path": "alpha", "_host_session_key": "internal-host-key"}),
    );
    assert_eq!(selected["ok"], true, "{selected}");
    let public_args = json!({
        "tool_name": "exec_command",
        "permission": "network",
        "reason": "test reply shape without executing a command",
        "arguments": {"cmd": "pwd", "nested_user_data": "must remain unchanged"}
    });
    let mut host_args = public_args.clone();
    host_args["_host_session_key"] = json!("internal-host-key");
    let result = call_tool(&ctx, "request_permissions", &host_args);
    let requested = if mode == "dangerous" {
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["status"], "granted");
        &result["constraints"]["requested"]
    } else {
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["error"]["code"], "ELICITATION_UNSUPPORTED");
        &result["error"]["details"]["requested"]
    };
    assert_eq!(requested, &public_args, "internal routing metadata leaked into public permission reply: {result}");
}

#[test]
fn permission_grant_echo_excludes_internal_context() {
    assert_public_permission_echo("dangerous");
}

#[test]
fn permission_denial_echo_excludes_internal_context() {
    assert_public_permission_echo("trusted");
}
