mod common;

use std::fs;
use std::process::Command;

use coding_tools_mcp_desktop_lib::tools::list_tools_for_profile;
use common::*;
use serde_json::{json, Value};

#[cfg(windows)]
const TEST_PYTHON: &str = "python";
#[cfg(not(windows))]
const TEST_PYTHON: &str = "python3";

#[test]
fn server_info_returns_workspace_and_tools() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "server_info", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["server"], "coding-tools-mcp");
    assert_eq!(payload["version"], env!("CARGO_PKG_VERSION"));
    assert!(payload["tools"].is_array());
    assert!(payload["tool_count"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn explicit_relative_exec_workdir_is_resolved_from_active_project() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let project = temp.path().join("project-a");
    fs::create_dir_all(project.join("subdir")).expect("创建子目录");
    let ctx = ctx_for(temp.path());
    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-a", "_host_session_key": "session-a"}),
    ));

    let root = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": "pwd", "workdir": ".", "_host_session_key": "session-a"}),
    );
    let root_stdout = assert_ok(&root)["stdout"]
        .as_str()
        .unwrap_or("")
        .replace('\\', "/");
    assert!(root_stdout.contains("project-a"), "{root_stdout}");

    let subdir = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": "pwd", "workdir": "subdir", "_host_session_key": "session-a"}),
    );
    let subdir_stdout = assert_ok(&subdir)["stdout"]
        .as_str()
        .unwrap_or("")
        .replace('\\', "/");
    assert!(subdir_stdout.contains("project-a/subdir"), "{subdir_stdout}");
}

#[test]
fn read_file_happy_path() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "read_file", json!({"path": "src/math.js"}));
    let payload = assert_ok(&out);
    assert_eq!(payload["path"], "src/math.js");
    assert_eq!(payload["encoding"], "utf-8");
}

#[test]
fn unknown_tool_is_validation_error() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "definitely_not_a_tool", json!({}));
    let err = assert_err(&out);
    assert_eq!(err["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(err["error"]["category"], "validation");
}

#[test]
fn read_file_explicit_parent_path_is_read_only() {
    let fx = malicious_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "read_file", json!({"path": "../outside-secret.txt"}));
    let result = assert_ok(&out);
    assert!(result["content"]
        .as_str()
        .unwrap_or("")
        .contains("TOP_SECRET"));
}

#[test]
fn request_permissions_is_unsupported_not_silent_grant() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "request_permissions",
        json!({
            "tool_name": "exec_command",
            "permission": "network",
            "reason": "verify compliance denial shape",
            "arguments": {"cmd": "curl https://example.com"}
        }),
    );
    assert_err(&out);
    assert_eq!(out["error"]["code"], "ELICITATION_UNSUPPORTED");
    assert_eq!(out["status"], "unsupported");
}

#[test]
fn request_permissions_exposes_public_schema_and_grants_in_dangerous_mode() {
    let tools = list_tools_for_profile("core");
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "request_permissions")
        .expect("request_permissions descriptor");
    let schema = &tool["inputSchema"];
    assert_eq!(
        schema["required"],
        json!(["tool_name", "permission", "reason", "arguments"])
    );
    assert!(schema["properties"]["permission"]["enum"]
        .as_array()
        .expect("permission enum")
        .contains(&json!("network")));

    let fx = tiny_js_fixture();
    let mut ctx = ctx_for(&fx.root);
    ctx.permission_mode = "dangerous".into();
    ctx.policy.permission_mode = "dangerous".into();
    let args = json!({
        "tool_name": "exec_command",
        "permission": "network",
        "reason": "verify dangerous-mode compatibility",
        "arguments": {"cmd": "curl https://example.com"}
    });
    let out = invoke(&ctx, "request_permissions", args.clone());
    let payload = assert_ok(&out);
    assert_eq!(payload["status"], "granted");
    assert_eq!(payload["constraints"]["mode"], "dangerous");
    assert_eq!(payload["constraints"]["requested"], args);
}

#[test]
fn check_exec_environment_reports_policy_metadata() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "check_exec_environment", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["permission_mode"], "trusted");
    assert!(payload["allowed_commands"].is_array());
}

#[test]
fn default_cwd_is_used_by_file_and_native_exec_tools() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    assert_ok(&invoke(&ctx, "set_default_cwd", json!({"path": "src"})));

    let file_result = invoke(&ctx, "read_file", json!({"path": "math.js"}));
    let file = assert_ok(&file_result);
    assert_eq!(file["path"], "src/math.js");

    let pwd_result = invoke(&ctx, "exec_command", json!({"cmd": "pwd"}));
    let pwd = assert_ok(&pwd_result);
    assert!(pwd["stdout"].as_str().unwrap_or("").contains("src"));
}

#[test]
fn active_project_is_isolated_per_host_session_and_accepts_absolute_paths() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let root = temp.path();
    let project_a = root.join("project-a");
    let project_b = root.join("project-b");
    fs::create_dir_all(&project_a).expect("创建 project-a");
    fs::create_dir_all(&project_b).expect("创建 project-b");
    fs::write(project_a.join("name.txt"), "A\n").expect("写入 A");
    fs::write(project_b.join("name.txt"), "B\n").expect("写入 B");

    let ctx = ctx_for(root);
    let a = invoke(
        &ctx,
        "set_active_project",
        json!({
            "path": project_a.display().to_string(),
            "_host_session_key": "session-a"
        }),
    );
    assert_eq!(assert_ok(&a)["active_project"], "project-a");

    let b = invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-b", "_host_session_key": "session-b"}),
    );
    assert_eq!(assert_ok(&b)["active_project"], "project-b");

    let read_a = invoke(
        &ctx,
        "read_file",
        json!({"path": "name.txt", "_host_session_key": "session-a"}),
    );
    assert_eq!(assert_ok(&read_a)["content"], "A\n");
    let read_b = invoke(
        &ctx,
        "read_file",
        json!({"path": "name.txt", "_host_session_key": "session-b"}),
    );
    assert_eq!(assert_ok(&read_b)["content"], "B\n");

    let active_a = invoke(
        &ctx,
        "get_active_project",
        json!({"_host_session_key": "session-a"}),
    );
    let active_b = invoke(
        &ctx,
        "get_active_project",
        json!({"_host_session_key": "session-b"}),
    );
    assert_eq!(assert_ok(&active_a)["active_project"], "project-a");
    assert_eq!(assert_ok(&active_b)["active_project"], "project-b");
}

#[test]
fn history_archives_follow_session_active_project_and_remain_isolated() {
    let temp = tempfile::tempdir().expect("创建 Workspace Pool");
    let root = temp.path();
    let project_a = root.join("project-a");
    let project_b = root.join("project-b");
    fs::create_dir_all(&project_a).expect("创建 project-a");
    fs::create_dir_all(&project_b).expect("创建 project-b");
    let ctx = ctx_for(root);

    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-a", "_host_session_key": "session-a"}),
    ));
    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-b", "_host_session_key": "session-b"}),
    ));

    let boot_a = invoke(
        &ctx,
        "history_session_bootstrap",
        json!({
            "workspace_root": root.display().to_string(),
            "initial_user_input": "PROJECT_A_FIRST",
            "_host_session_key": "session-a"
        }),
    );
    let boot_a = assert_ok(&boot_a);
    assert_eq!(boot_a["history_scope"], "active_project");
    assert_eq!(boot_a["current_number"], 1);
    assert_eq!(boot_a["current_path"], "docs/history-session/1.md");
    assert!(boot_a["project_root"]
        .as_str()
        .unwrap_or("")
        .contains("project-a"));

    let boot_b = invoke(
        &ctx,
        "history_session_bootstrap",
        json!({
            "workspace_root": root.display().to_string(),
            "initial_user_input": "PROJECT_B_FIRST",
            "_host_session_key": "session-b"
        }),
    );
    let boot_b = assert_ok(&boot_b);
    assert_eq!(boot_b["history_scope"], "active_project");
    assert_eq!(boot_b["current_number"], 1);
    assert_eq!(boot_b["current_path"], "docs/history-session/1.md");
    assert!(boot_b["project_root"]
        .as_str()
        .unwrap_or("")
        .contains("project-b"));

    assert_ok(&invoke(
        &ctx,
        "history_session_checkpoint",
        json!({
            "session_key": "session-a",
            "expected_path": "docs/history-session/1.md",
            "project_id": boot_a["project_id"],
            "raw_user_input": "PROJECT_A_NEXT",
            "_host_session_key": "session-a"
        }),
    ));
    assert_ok(&invoke(
        &ctx,
        "history_session_checkpoint",
        json!({
            "session_key": "session-b",
            "expected_path": "docs/history-session/1.md",
            "project_id": boot_b["project_id"],
            "raw_user_input": "PROJECT_B_NEXT",
            "_host_session_key": "session-b"
        }),
    ));

    let history_a = fs::read_to_string(project_a.join("docs/history-session/1.md"))
        .expect("读取 project-a history");
    let history_b = fs::read_to_string(project_b.join("docs/history-session/1.md"))
        .expect("读取 project-b history");
    assert!(history_a.contains("PROJECT_A_FIRST"));
    assert!(history_a.contains("PROJECT_A_NEXT"));
    assert!(!history_a.contains("PROJECT_B_FIRST"));
    assert!(history_b.contains("PROJECT_B_FIRST"));
    assert!(history_b.contains("PROJECT_B_NEXT"));
    assert!(!history_b.contains("PROJECT_A_FIRST"));
    assert!(!root.join("docs/history-session").exists());
}

#[test]
fn harness_tasks_are_scoped_to_active_project_identity() {
    let temp = tempfile::tempdir().expect("创建 Workspace Pool");
    let root = temp.path();
    fs::create_dir_all(root.join("project-a")).expect("创建 project-a");
    fs::create_dir_all(root.join("project-b")).expect("创建 project-b");
    fs::write(root.join("project-a/a.txt"), "A\n").expect("写 project-a");
    fs::write(root.join("project-b/b.txt"), "B\n").expect("写 project-b");
    let ctx = ctx_for(root);

    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-a", "_host_session_key": "session-a"}),
    ));
    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-b", "_host_session_key": "session-b"}),
    ));

    let start_a = invoke(
        &ctx,
        "start_task",
        json!({"objective": "task-a", "_host_session_key": "session-a"}),
    );
    let task_a = assert_ok(&start_a)["task"].clone();
    let start_b = invoke(
        &ctx,
        "start_task",
        json!({"objective": "task-b", "_host_session_key": "session-b"}),
    );
    let task_b = assert_ok(&start_b)["task"].clone();

    assert_ne!(task_a["workspace_id"], task_b["workspace_id"]);
    assert_ne!(task_a["id"], task_b["id"]);

    let status_a = invoke(
        &ctx,
        "harness_status",
        json!({"_host_session_key": "session-a"}),
    );
    let status_b = invoke(
        &ctx,
        "harness_status",
        json!({"_host_session_key": "session-b"}),
    );
    assert_eq!(assert_ok(&status_a)["task_id"], task_a["id"]);
    assert_eq!(assert_ok(&status_b)["task_id"], task_b["id"]);

    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-b", "_host_session_key": "session-a"}),
    ));
    let switched = invoke(
        &ctx,
        "harness_status",
        json!({"_host_session_key": "session-a"}),
    );
    assert_eq!(assert_ok(&switched)["task_id"], task_b["id"]);

    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-a", "_host_session_key": "session-a"}),
    ));
    let restored = invoke(
        &ctx,
        "harness_status",
        json!({"_host_session_key": "session-a"}),
    );
    assert_eq!(assert_ok(&restored)["task_id"], task_a["id"]);
}

#[test]
fn harness_operation_log_follows_session_active_project() {
    let temp = tempfile::tempdir().expect("创建 Workspace Pool");
    let root = temp.path();
    fs::create_dir_all(root.join("project-a")).expect("创建 project-a");
    fs::create_dir_all(root.join("project-b")).expect("创建 project-b");
    let ctx = ctx_for(root);

    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-a", "_host_session_key": "session-a"}),
    ));
    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-b", "_host_session_key": "session-b"}),
    ));

    assert_ok(&invoke(
        &ctx,
        "exec_command",
        json!({"cmd": "pwd", "_host_session_key": "session-a"}),
    ));
    assert_ok(&invoke(
        &ctx,
        "exec_command",
        json!({"cmd": "pwd", "_host_session_key": "session-b"}),
    ));

    let log_a = invoke(
        &ctx,
        "operation_log",
        json!({"_host_session_key": "session-a"}),
    );
    let log_b = invoke(
        &ctx,
        "operation_log",
        json!({"_host_session_key": "session-b"}),
    );
    let operations_a = assert_ok(&log_a)["operations"].as_array().unwrap();
    let operations_b = assert_ok(&log_b)["operations"].as_array().unwrap();
    assert!(!operations_a.is_empty());
    assert!(!operations_b.is_empty());
    assert!(operations_a.iter().all(|operation| operation["tool"] == "exec_command"));
    assert!(operations_b.iter().all(|operation| operation["tool"] == "exec_command"));
    assert_ne!(operations_a[0]["workspace_id"], operations_b[0]["workspace_id"]);
}

#[test]
fn active_project_rejects_absolute_paths_outside_workspace_pool() {
    let pool = tempfile::tempdir().expect("创建 Workspace Pool");
    let outside = tempfile::tempdir().expect("创建外部目录");
    let ctx = ctx_for(pool.path());
    let result = invoke(
        &ctx,
        "set_active_project",
        json!({
            "path": outside.path().display().to_string(),
            "_host_session_key": "session-a"
        }),
    );
    let error = assert_err(&result);
    assert_eq!(error["error"]["code"], "PATH_OUTSIDE_WORKSPACE");
}

#[test]
fn active_project_root_keeps_catastrophic_delete_floor() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let project = temp.path().join("project-a");
    fs::create_dir_all(&project).expect("创建项目目录");
    let mut ctx = ctx_for(temp.path());
    ctx.permission_mode = "dangerous".into();
    ctx.policy.permission_mode = "dangerous".into();

    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "project-a", "_host_session_key": "session-a"}),
    ));
    let blocked = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": "rm -rf .", "_host_session_key": "session-a"}),
    );
    let error = assert_err(&blocked);
    assert!(error["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("active project root"));
}

#[test]
fn discover_projects_supports_marker_scan_and_name_lookup() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let marked = temp.path().join("marked-repo");
    let plain = temp.path().join("逆向环境").join("utools");
    fs::create_dir_all(marked.join(".git")).expect("创建 marker");
    fs::create_dir_all(&plain).expect("创建 plain project");
    let ctx = ctx_for(temp.path());

    let all = invoke(&ctx, "discover_projects", json!({"max_depth": 3}));
    let projects = assert_ok(&all)["projects"].as_array().unwrap().clone();
    assert!(projects.iter().any(|project| project["path"] == "marked-repo"));

    let by_name = invoke(
        &ctx,
        "discover_projects",
        json!({"query": "utools", "max_depth": 3}),
    );
    let projects = assert_ok(&by_name)["projects"].as_array().unwrap().clone();
    assert_eq!(projects.len(), 1);
    assert!(projects[0]["path"].as_str().unwrap_or("").contains("utools"));
}

#[test]
fn git_tools_follow_session_active_project_inside_workspace_pool() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let root = temp.path();
    let repo_a = root.join("repo-a");
    let repo_b = root.join("repo-b");
    for (repo, content, message) in [
        (&repo_a, "alpha\n", "commit-a"),
        (&repo_b, "beta\n", "commit-b"),
    ] {
        fs::create_dir_all(repo).expect("创建 repo");
        fs::write(repo.join("README.md"), content).expect("写 README");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "测试用户"],
            vec!["add", "README.md"],
            vec!["commit", "-q", "-m", message],
        ] {
            let output = Command::new("git")
                .current_dir(repo)
                .args(args)
                .output()
                .expect("执行 git");
            assert!(output.status.success(), "git 命令失败: {:?}", output);
        }
    }

    fs::write(repo_a.join("README.md"), "alpha changed\n").expect("修改 repo-a");
    let ctx = ctx_for(root);
    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "repo-a", "_host_session_key": "session-a"}),
    ));
    assert_ok(&invoke(
        &ctx,
        "set_active_project",
        json!({"path": "repo-b", "_host_session_key": "session-b"}),
    ));

    let log_a = invoke(
        &ctx,
        "git_log",
        json!({"_host_session_key": "session-a", "max_count": 3}),
    );
    assert_eq!(assert_ok(&log_a)["commits"][0]["subject"], "commit-a");
    let log_b = invoke(
        &ctx,
        "git_log",
        json!({"_host_session_key": "session-b", "max_count": 3}),
    );
    assert_eq!(assert_ok(&log_b)["commits"][0]["subject"], "commit-b");

    let diff_a = invoke(
        &ctx,
        "git_diff",
        json!({"_host_session_key": "session-a"}),
    );
    assert!(assert_ok(&diff_a)["diff"]
        .as_str()
        .unwrap_or("")
        .contains("alpha changed"));
    let diff_b = invoke(
        &ctx,
        "git_diff",
        json!({"_host_session_key": "session-b"}),
    );
    assert_eq!(assert_ok(&diff_b)["diff"], "");
}

#[test]
fn git_log_root_does_not_pass_empty_pathspec() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("repo");
    fs::create_dir_all(&workspace).expect("创建仓库目录");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");

    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "测试用户"],
        vec!["add", "README.md"],
        vec!["commit", "-q", "-m", "初始化"],
    ] {
        let output = Command::new("git")
            .current_dir(&workspace)
            .args(args)
            .output()
            .expect("执行 git");
        assert!(output.status.success(), "git 命令失败: {:?}", output);
    }

    let ctx = ctx_for(&workspace);
    let result = invoke(&ctx, "git_log", json!({"path": ".", "max_count": 3}));
    let payload = assert_ok(&result);
    assert_eq!(payload["is_repo"], true);
    assert_eq!(payload["commits"].as_array().unwrap().len(), 1);
    for commit in payload["commits"].as_array().unwrap() {
        for field in [
            "hash",
            "short_hash",
            "author_name",
            "author_email",
            "author_date",
            "subject",
        ] {
            assert_eq!(
                commit[field].as_str().unwrap(),
                commit[field].as_str().unwrap().trim()
            );
        }
    }
}

#[test]
fn advanced_profile_exposes_every_declared_tool() {
    let declared = coding_tools_mcp_desktop_lib::tools::registry::P0_TOOLS
        .iter()
        .map(|(name, ..)| *name)
        .collect::<std::collections::HashSet<_>>();
    let tool_values = coding_tools_mcp_desktop_lib::tools::list_tools_for_profile("advanced");
    let exposed = tool_values
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(declared, exposed);
    assert!(declared
        .iter()
        .all(|name| coding_tools_mcp_desktop_lib::tools::is_allowed_tool(name)));
}

#[test]
fn core_profile_keeps_the_default_capabilities_and_adds_history_tools() {
    let tools = coding_tools_mcp_desktop_lib::tools::list_tools_for_profile("core");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::HashSet<_>>();
    let expected = coding_tools_mcp_desktop_lib::tools::registry::CORE_TOOLS
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names, expected);
    assert_eq!(names.len(), 29);
    assert!(names.contains("grep_text"));
    assert!(names.contains("get_active_project"));
    assert!(names.contains("set_active_project"));
    assert!(names.contains("discover_projects"));
    assert!(names.contains("history_session_bootstrap"));
    assert!(names.contains("history_session_checkpoint"));
    assert!(names.contains("history_session_validate"));
    assert!(names.contains("history_session_search"));
    assert!(names.contains("history_session_read"));
    assert!(!names.contains("harness_status"));
    assert!(!names.contains("start_task"));
}

#[test]
fn exec_health_check_reports_worker_and_pipe_status() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "exec_health_check", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["worker"]["alive"], true);
    assert_eq!(payload["session_create"], true);
    assert_eq!(payload["command_run"], true);
    assert_eq!(payload["stdout_capture"], true);
    assert_eq!(payload["stderr_capture"], true);
}

#[test]
fn native_diagnostics_support_pwd_and_ls_without_a_shell() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let pwd_result = invoke(&ctx, "exec_command", json!({"cmd": "pwd"}));
    let pwd = assert_ok(&pwd_result);
    assert_eq!(pwd["command"], "pwd");
    assert!(pwd["stdout"]
        .as_str()
        .unwrap_or("")
        .contains("tiny-js-project"));
    assert_eq!(pwd["execution_mode"], "native_builtin");
    assert_eq!(pwd["harness_mode"], "standalone");
    assert_eq!(pwd["task_required"], false);
    assert_eq!(pwd["command_runner"], "native_builtin");
    assert_eq!(pwd["status"], "exited");
    assert_eq!(pwd["exit_code"], 0);
    assert_eq!(pwd["transport_ok"], true);
    assert_eq!(pwd["command_ok"], true);
    assert_eq!(pwd["duration_ms"], 0);
    assert_eq!(pwd["elapsed_ms"], 0);
    assert!(pwd["stdout"].is_string());
    assert_eq!(pwd["stderr"], "");

    let ls_result = invoke(&ctx, "exec_command", json!({"cmd": "ls"}));
    let ls = assert_ok(&ls_result);
    assert!(ls["stdout"].as_str().unwrap_or("").contains("src"));
    assert_eq!(ls["exit_code"], 0);
}

#[test]
fn direct_exec_uses_the_same_result_contract() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} --version"),
            "filesystem_scope": "workspace",
            "yield_time_ms": 10_000
        }),
    );
    let payload = assert_ok(&result);

    assert_eq!(payload["command"], format!("{TEST_PYTHON} --version"));
    assert_eq!(payload["execution_mode"], "direct");
    assert_eq!(payload["harness_mode"], "standalone");
    assert_eq!(payload["task_required"], false);
    assert_eq!(payload["status"], "exited");
    assert_eq!(payload["exit_code"], 0);
    assert!(payload["stdout"].is_string());
    assert!(payload["stderr"].is_string());
    assert!(payload["duration_ms"].is_u64());
    assert_eq!(payload["duration_ms"], payload["elapsed_ms"]);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], true);
}

#[test]
fn nonzero_command_exit_keeps_transport_ok_but_sets_command_ok_false() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import sys; sys.exit(1)\""),
            "filesystem_scope": "workspace",
            "yield_time_ms": 10_000
        }),
    );
    let payload = assert_ok(&result);

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], false);
    assert_eq!(payload["status"], "exited");
    assert_eq!(payload["exit_code"], 1);
}

#[test]
fn retained_session_timeout_stops_the_process_after_deadline() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(2)\""),
            "filesystem_scope": "workspace",
            "timeout_ms": 100,
            "yield_time_ms": 0
        }),
    );
    let payload = assert_ok(&result);
    assert_eq!(payload["status"], "running");
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], Value::Null);
    assert_eq!(payload["stdin_open"], true);
    let session_id = payload["session_id"].as_str().expect("session id");

    std::thread::sleep(std::time::Duration::from_millis(250));
    let after = invoke(
        &ctx,
        "write_stdin",
        json!({"session_id": session_id, "chars": ""}),
    );
    assert_eq!(after["termination_reason"], "timeout");
    assert_eq!(after["status"], "exited");
    assert_eq!(after["transport_ok"], true);
    assert_eq!(after["command_ok"], false);
    assert_eq!(after["stdin_open"], false);
    #[cfg(unix)]
    assert_eq!(after["exit_code"], Value::Null);
}

#[test]
fn killed_session_reports_command_failure_even_when_transport_succeeds() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(2)\""),
            "filesystem_scope": "workspace",
            "timeout_ms": 10_000,
            "yield_time_ms": 0
        }),
    );
    let payload = assert_ok(&result);
    let session_id = payload["session_id"].as_str().expect("session id");

    let killed = invoke(
        &ctx,
        "kill_session",
        json!({"session_id": session_id, "wait_ms": 2_000}),
    );
    let killed = assert_ok(&killed);
    assert_eq!(killed["status"], "killed");
    assert_eq!(killed["killed"], true);
    assert_eq!(killed["transport_ok"], true);
    assert_eq!(killed["command_ok"], false);
    #[cfg(unix)]
    assert_eq!(killed["exit_code"], Value::Null);
}

#[test]
fn list_files_accepts_glob_alias() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "list_files",
        json!({"glob": "**/*.js", "max_results": 10}),
    );
    let payload = assert_ok(&out);
    let files = payload["files"].as_array().expect("files array");
    assert!(!files.is_empty());
    assert!(files
        .iter()
        .all(|f| f["path"].as_str().unwrap_or("").ends_with(".js")));
}

#[test]
fn search_text_filters_by_glob() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let hit = invoke(
        &ctx,
        "search_text",
        json!({"query": "function add", "glob": "**/*.js", "max_results": 10}),
    );
    let hit_payload = assert_ok(&hit);
    assert!(hit_payload["total_matches"].as_u64().unwrap_or(0) > 0);

    let miss = invoke(
        &ctx,
        "search_text",
        json!({"query": "function add", "glob": "**/*.py"}),
    );
    let miss_payload = assert_ok(&miss);
    assert_eq!(miss_payload["total_matches"].as_u64().unwrap_or(1), 0);
}

#[test]
fn search_text_skips_binary_and_oversized_files() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    // Fixture materializes assets/raw.bin with NUL bytes; must not match or thrash.
    let binary = invoke(
        &ctx,
        "search_text",
        json!({"query": "binary", "glob": "assets/**", "max_results": 10}),
    );
    let binary_payload = assert_ok(&binary);
    assert_eq!(binary_payload["total_matches"].as_u64().unwrap_or(1), 0);
    assert!(binary_payload["skipped_binary_files"].as_u64().unwrap_or(0) >= 1);

    // Oversized text file is skipped by max_file_bytes without being fully loaded.
    let large_path = fx.root.join("search/huge.txt");
    fs::write(&large_path, format!("needle {}\n", "x".repeat(4096))).expect("write huge");
    let oversized = invoke(
        &ctx,
        "search_text",
        json!({
            "query": "needle",
            "glob": "search/huge.txt",
            "max_file_bytes": 64,
            "max_results": 10
        }),
    );
    let oversized_payload = assert_ok(&oversized);
    assert_eq!(oversized_payload["total_matches"].as_u64().unwrap_or(1), 0);
    assert!(
        oversized_payload["skipped_large_files"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(oversized_payload["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w.as_str().unwrap_or("").contains("max_file_bytes")));
}

#[test]
fn search_text_stops_after_max_results() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "search_text",
        json!({"query": "common-token", "glob": "search/**", "max_results": 2}),
    );
    let payload = assert_ok(&out);
    assert_eq!(
        payload["matches"].as_array().map(|a| a.len()).unwrap_or(0),
        2
    );
    assert!(payload["truncated"].as_bool().unwrap_or(false));
    assert!(payload["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w.as_str().unwrap_or("").contains("result limit reached")));
}

#[test]
fn grep_reuses_search_text_schema_and_behavior() {
    let schema = coding_tools_mcp_desktop_lib::tools::registry::input_schema("grep");
    assert_eq!(
        schema,
        coding_tools_mcp_desktop_lib::tools::registry::input_schema("search_text")
    );

    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    assert_ok(&invoke(&ctx, "set_default_cwd", json!({"path": "src"})));
    let output = invoke(
        &ctx,
        "grep",
        json!({
            "query": "function\\s+add",
            "path": ".",
            "glob": "**/*.js",
            "regex": true,
            "case_sensitive": true,
            "max_results": 10
        }),
    );
    let payload = assert_ok(&output);
    let matches = payload["matches"].as_array().expect("matches array");
    assert!(!matches.is_empty());
    assert!(matches
        .iter()
        .all(|item| item["path"].as_str().unwrap_or("").starts_with("src/")));
}
