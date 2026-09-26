use std::fs;
use std::path::{Path, PathBuf};

use coding_tools_mcp_desktop_lib::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

fn dangerous_context(pool: &Path, harness: &Path) -> ToolContext {
    let mut ctx = ToolContext::for_test(pool.to_path_buf(), harness.to_path_buf())
        .expect("tool context");
    ctx.permission_mode = "dangerous".into();
    ctx.policy.permission_mode = "dangerous".into();
    ctx
}

#[test]
fn same_project_reselection_refreshes_a_stale_alias_path() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    fs::create_dir_all(pool.join("alpha")).unwrap();
    fs::write(pool.join("alpha/name.txt"), "alpha").unwrap();
    let target = pool.join("alpha").canonicalize().unwrap();
    let alias = pool.join("alpha-link");
    if make_dir_symlink(&target, &alias).is_err() {
        eprintln!("UNTESTED: OS refused directory symlink creation");
        return;
    }
    let ctx = dangerous_context(&pool, &harness);
    ok(call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "alpha-link"}),
    ));
    fs::remove_dir(&alias).unwrap();

    let refreshed = ok(call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "alpha"}),
    ));
    assert_eq!(refreshed["already_bound"], true);
    assert_eq!(
        ok(call(&ctx, "chat", "read_file", json!({"path": "name.txt"})))["content"],
        "alpha"
    );
}

fn ok(value: Value) -> Value {
    assert_eq!(value["ok"], true, "{value}");
    value
}

fn call(ctx: &ToolContext, session: &str, tool: &str, mut args: Value) -> Value {
    args["_host_session_key"] = json!(session);
    call_tool(ctx, tool, &args)
}

fn find_binding_file(root: &Path, session_key: &str) -> PathBuf {
    fn walk(dir: &Path, session_key: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = walk(&path, session_key) {
                    return Some(found);
                }
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if text.contains(session_key) {
                return Some(path);
            }
        }
        None
    }
    walk(root, session_key).expect("session binding file")
}

fn make_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

#[test]
fn host_session_without_binding_never_falls_back_to_default_cwd() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    fs::create_dir_all(pool.join("beta")).unwrap();
    fs::write(pool.join("beta/name.txt"), "beta").unwrap();
    let ctx = dangerous_context(&pool, &harness);
    ctx.set_default_cwd(pool.join("beta").canonicalize().unwrap());

    let state = ok(call(&ctx, "chat", "get_active_project", json!({})));
    assert_eq!(state["source"], "unbound");
    assert!(state["active_project"].is_null());
    assert_eq!(state["requires_binding"], true);
    assert!(state.get("default_cwd").is_none());

    let read = call(&ctx, "chat", "read_file", json!({"path": "name.txt"}));
    assert_eq!(read["ok"], false, "{read}");
    assert_eq!(read["error"]["code"], "ACTIVE_PROJECT_REQUIRED");
}

#[test]
fn project_switch_requires_explicit_rebind_flag() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    for project in ["alpha", "beta"] {
        fs::create_dir_all(pool.join(project)).unwrap();
        fs::write(pool.join(project).join("name.txt"), project).unwrap();
    }
    let ctx = dangerous_context(&pool, &harness);
    ok(call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "alpha"}),
    ));

    let accidental = call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "beta"}),
    );
    assert_eq!(accidental["ok"], false, "{accidental}");
    assert_eq!(
        accidental["error"]["code"],
        "ACTIVE_PROJECT_REBIND_REQUIRED"
    );
    assert_eq!(
        ok(call(&ctx, "chat", "read_file", json!({"path": "name.txt"})))["content"],
        "alpha"
    );

    let switched = ok(call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "beta", "allow_rebind": true}),
    ));
    assert_eq!(switched["rebound"], true);
    assert_eq!(
        ok(call(&ctx, "chat", "read_file", json!({"path": "name.txt"})))["content"],
        "beta"
    );
}

#[test]
fn corrupt_one_session_binding_stays_closed_after_another_session_repairs_itself() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    for project in ["alpha", "beta"] {
        fs::create_dir_all(pool.join(project)).unwrap();
    }

    {
        let ctx = dangerous_context(&pool, &harness);
        ok(call(
            &ctx,
            "old-chat",
            "set_active_project",
            json!({"path": "alpha"}),
        ));
    }
    let state_file = find_binding_file(&harness.join("active-project-sessions"), "old-chat");
    fs::write(&state_file, b"{ not valid json").unwrap();

    let ctx2 = dangerous_context(&pool, &harness);
    let closed = call(&ctx2, "old-chat", "get_active_project", json!({}));
    assert_eq!(closed["ok"], false, "{closed}");
    assert_eq!(
        closed["error"]["code"],
        "ACTIVE_PROJECT_STORE_UNAVAILABLE"
    );
    ok(call(
        &ctx2,
        "repair-chat",
        "set_active_project",
        json!({"path": "beta"}),
    ));

    let ctx3 = dangerous_context(&pool, &harness);
    ctx3.set_default_cwd(pool.join("beta").canonicalize().unwrap());
    let still_closed = call(&ctx3, "old-chat", "get_active_project", json!({}));
    assert_eq!(still_closed["ok"], false, "{still_closed}");
    assert_eq!(
        still_closed["error"]["code"],
        "ACTIVE_PROJECT_STORE_UNAVAILABLE"
    );
}

#[test]
fn separate_contexts_do_not_clobber_other_session_bindings() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    for project in ["alpha", "beta"] {
        fs::create_dir_all(pool.join(project)).unwrap();
    }
    let ctx_a = dangerous_context(&pool, &harness);
    let ctx_b = dangerous_context(&pool, &harness);
    ok(call(
        &ctx_a,
        "s1",
        "set_active_project",
        json!({"path": "alpha"}),
    ));
    ok(call(
        &ctx_b,
        "s2",
        "set_active_project",
        json!({"path": "beta"}),
    ));

    let ctx_c = dangerous_context(&pool, &harness);
    assert_eq!(
        ok(call(&ctx_c, "s1", "get_active_project", json!({})))["active_project"],
        "alpha"
    );
    assert_eq!(
        ok(call(&ctx_c, "s2", "get_active_project", json!({})))["active_project"],
        "beta"
    );
}

#[test]
fn custom_history_path_cannot_exempt_real_source_code() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    fs::create_dir_all(pool.join("alpha/src")).unwrap();
    fs::write(pool.join("alpha/src/code.rs"), "fn real() {}\n").unwrap();
    let ctx = dangerous_context(&pool, &harness);
    ok(call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "alpha"}),
    ));

    let search = call(
        &ctx,
        "chat",
        "history_session_search",
        json!({"history_dir": "src", "query": "nothing"}),
    );
    assert_eq!(search["ok"], false, "{search}");
    assert_eq!(search["error"]["code"], "HISTORY_DIR_FIXED");

    ok(call(
        &ctx,
        "chat",
        "start_task",
        json!({"objective": "baseline"}),
    ));
    fs::write(
        pool.join("alpha/src/code.rs"),
        "fn real() { changed(); }\n",
    )
    .unwrap();
    let result = call(&ctx, "chat", "exec_command", json!({"cmd": "pwd"}));
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["code"], "FILE_CHANGED_EXTERNALLY");
}

#[test]
fn readonly_history_search_does_not_write_managed_path_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    fs::create_dir_all(pool.join("alpha")).unwrap();
    fs::create_dir_all(&harness).unwrap();
    fs::write(harness.join("managed-project-paths"), b"blocked legacy path").unwrap();
    let ctx = dangerous_context(&pool, &harness);
    ok(call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "alpha"}),
    ));
    let result = call(
        &ctx,
        "chat",
        "history_session_search",
        json!({"query": ""}),
    );
    assert_eq!(result["ok"], true, "{result}");
}

#[test]
fn workspace_root_project_uses_fixed_history_without_baseline_self_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    fs::create_dir_all(&pool).unwrap();
    fs::write(pool.join("code.txt"), "code").unwrap();
    let ctx = dangerous_context(&pool, &harness);
    ok(call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "."}),
    ));
    let boot = ok(call(
        &ctx,
        "chat",
        "history_session_bootstrap",
        json!({"initial_user_input": "INITIAL"}),
    ));
    ok(call(
        &ctx,
        "chat",
        "start_task",
        json!({"objective": "project work"}),
    ));
    ok(call(
        &ctx,
        "chat",
        "history_session_checkpoint",
        json!({
            "session_key": boot["session_key"],
            "expected_path": boot["current_path"],
            "project_id": boot["project_id"],
            "raw_user_input": "save progress"
        }),
    ));
    let result = call(&ctx, "chat", "exec_command", json!({"cmd": "pwd"}));
    assert_eq!(result["ok"], true, "{result}");
}

#[test]
fn bound_path_retargeted_by_symlink_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    for project in ["alpha", "beta"] {
        fs::create_dir_all(pool.join(project)).unwrap();
        fs::write(pool.join(project).join("name.txt"), project).unwrap();
    }
    let ctx = dangerous_context(&pool, &harness);
    ok(call(
        &ctx,
        "chat",
        "set_active_project",
        json!({"path": "alpha"}),
    ));
    fs::remove_dir_all(pool.join("alpha")).unwrap();
    let beta = pool.join("beta").canonicalize().unwrap();
    if make_dir_symlink(&beta, &pool.join("alpha")).is_err() {
        eprintln!("UNTESTED: OS refused directory symlink creation");
        return;
    }
    let result = call(&ctx, "chat", "read_file", json!({"path": "name.txt"}));
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["code"], "ACTIVE_PROJECT_TARGET_CHANGED");
}
