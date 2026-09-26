//! Isolated reproductions for the 2026-09-26 Phase 2 review.
//! Pending design findings are ignored in normal runs, NOT accepted as fixed.
//! Run them explicitly with: cargo test --test phase2_static_review pending_ -- --ignored --nocapture

use std::fs;
use std::path::PathBuf;

use coding_tools_mcp_desktop_lib::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

struct Fixture {
    ctx: ToolContext,
    pool: PathBuf,
    _temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("isolated fixture");
        let pool = temp.path().join("pool");
        for project in ["alpha", "beta"] {
            fs::create_dir_all(pool.join(project)).unwrap();
            fs::write(pool.join(project).join("name.txt"), project).unwrap();
            fs::write(pool.join(project).join("edit.txt"), "before\n").unwrap();
        }
        fs::write(pool.join("edit.txt"), "before\n").unwrap();
        let mut ctx = ToolContext::for_test(pool.clone(), temp.path().join("harness")).unwrap();
        ctx.permission_mode = "dangerous".into();
        ctx.policy.permission_mode = "dangerous".into();
        Self {
            ctx,
            pool,
            _temp: temp,
        }
    }

    fn call(&self, tool: &str, mut args: Value) -> Value {
        args["_host_session_key"] = json!("review-chat");
        call_tool(&self.ctx, tool, &args)
    }

    fn bind(&self, project: &str) {
        ok(self.call("set_active_project", json!({"path": project})));
    }

    fn bootstrap(&self, input: &str) -> Value {
        ok(self.call(
            "history_session_bootstrap",
            json!({"initial_user_input": input}),
        ))
    }
}

fn ok(value: Value) -> Value {
    assert_eq!(value["ok"], true, "{value}");
    value
}

#[test]
fn missing_search_path_defaults_to_the_active_project() {
    let f = Fixture::new();
    for path in ["pool.txt", "alpha/hit.txt", "beta/hit.txt"] {
        fs::write(f.pool.join(path), "unique-review-marker\n").unwrap();
    }
    f.bind("alpha");
    for tool in ["search_text", "grep_text", "grep"] {
        let value = ok(f.call(tool, json!({"query": "unique-review-marker"})));
        let paths: Vec<_> = value["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            vec!["alpha/hit.txt"],
            "{tool} searched outside its default project"
        );
    }
}

#[test]
fn codex_add_patch_uses_project_root_not_pool_root() {
    let f = Fixture::new();
    f.bind("alpha");
    ok(f.call(
        "apply_patch",
        json!({"patch": "*** Begin Patch\n*** Add File: added.txt\n+hello\n*** End Patch\n"}),
    ));
    assert!(
        !f.pool.join("added.txt").exists(),
        "patch wrote into the pool root"
    );
    assert_eq!(
        fs::read_to_string(f.pool.join("alpha/added.txt")).unwrap(),
        "hello\n"
    );
}

#[test]
fn codex_update_patch_uses_project_root_not_pool_root() {
    let f = Fixture::new();
    f.bind("alpha");
    ok(f.call("apply_patch", json!({"patch": "*** Begin Patch\n*** Update File: edit.txt\n@@\n-before\n+after\n*** End Patch\n"})));
    assert_eq!(
        fs::read_to_string(f.pool.join("edit.txt")).unwrap(),
        "before\n",
        "pool file changed"
    );
    assert_eq!(
        fs::read_to_string(f.pool.join("alpha/edit.txt")).unwrap(),
        "after\n"
    );
    assert_eq!(
        fs::read_to_string(f.pool.join("beta/edit.txt")).unwrap(),
        "before\n"
    );
}

#[test]
fn codex_delete_patch_uses_project_root_not_pool_root() {
    let f = Fixture::new();
    f.bind("alpha");
    ok(f.call(
        "apply_patch",
        json!({"patch": "*** Begin Patch\n*** Delete File: edit.txt\n*** End Patch\n"}),
    ));
    assert!(f.pool.join("edit.txt").exists(), "pool file deleted");
    assert!(!f.pool.join("alpha/edit.txt").exists());
    assert!(f.pool.join("beta/edit.txt").exists());
}

#[test]
fn project_selection_can_recover_after_project_directory_is_renamed() {
    let f = Fixture::new();
    f.bind("alpha");
    fs::rename(f.pool.join("alpha"), f.pool.join("renamed-alpha")).unwrap();
    let result = f.call("set_active_project", json!({"path": "beta"}));
    assert_eq!(
        result["ok"], true,
        "recovery blocked by old project Harness: {result}"
    );
    let status = ok(f.call("get_active_project", json!({})));
    assert_eq!(status["active_project"], "beta");
}

#[test]
fn scoped_patch_preflight_does_not_write_any_project() {
    let f = Fixture::new();
    f.bind("alpha");
    let patch = "*** Begin Patch\n*** Add File: check.txt\n+preflight\n*** End Patch\n";
    for tool in ["apply_patch", "patch_check"] {
        let result = ok(f.call(tool, json!({"patch": patch, "dry_run": true})));
        assert_eq!(result["would_create"], json!(["alpha/check.txt"]));
        assert!(!f.pool.join("alpha/check.txt").exists());
        assert!(!f.pool.join("check.txt").exists());
    }
}

#[test]
fn codex_hunk_lines_that_look_like_unified_headers_are_unchanged() {
    let f = Fixture::new();
    fs::write(f.pool.join("alpha/edit.txt"), "-- a/original\n").unwrap();
    f.bind("alpha");
    let patch = "*** Begin Patch\n*** Update File: edit.txt\n@@\n--- a/original\n+++ b/replacement\n*** End Patch\n";
    ok(f.call("apply_patch", json!({"patch": patch})));
    assert_eq!(
        fs::read_to_string(f.pool.join("alpha/edit.txt")).unwrap(),
        "++ b/replacement\n"
    );
}

#[test]
fn codex_paths_preserve_real_a_and_b_directories() {
    let f = Fixture::new();
    fs::create_dir_all(f.pool.join("a/b")).unwrap();
    f.bind("a");
    ok(f.call(
        "apply_patch",
        json!({"patch": "*** Begin Patch\n*** Add File: b/nested.txt\n+data\n*** End Patch\n"}),
    ));
    assert!(f.pool.join("a/b/nested.txt").exists());
    assert!(!f.pool.join("b/nested.txt").exists());
}

#[test]
fn unified_patch_transport_headers_and_plain_headers_are_both_scoped() {
    for headers in [
        "--- a/edit.txt\n+++ b/edit.txt",
        "--- edit.txt\n+++ edit.txt",
    ] {
        let f = Fixture::new();
        f.bind("alpha");
        ok(f.call(
            "apply_patch",
            json!({"patch": format!("{headers}\n@@ -1 +1 @@\n-before\n+after\n")}),
        ));
        assert_eq!(
            fs::read_to_string(f.pool.join("alpha/edit.txt")).unwrap(),
            "after\n"
        );
        assert_eq!(
            fs::read_to_string(f.pool.join("edit.txt")).unwrap(),
            "before\n"
        );
    }
}

#[test]
fn scoped_patch_keeps_parent_traversal_rejected() {
    let f = Fixture::new();
    f.bind("alpha");
    let result = f.call(
        "apply_patch",
        json!({"patch": "*** Begin Patch\n*** Add File: ../outside.txt\n+bad\n*** End Patch\n"}),
    );
    assert_eq!(result["ok"], false, "{result}");
    assert!(!f.pool.parent().unwrap().join("outside.txt").exists());
    assert!(!f.pool.join("outside.txt").exists());
}

#[test]
#[ignore = "P1-HISTORY-TARGET: stable archive identity / switch lifecycle requires approval"]
fn pending_old_checkpoint_must_not_silently_write_to_another_project() {
    let f = Fixture::new();
    f.bind("alpha");
    let old_target = f.bootstrap("ALPHA-INITIAL");
    f.bind("beta");
    let new_target = f.bootstrap("BETA-INITIAL");
    assert_eq!(old_target["session_key"], new_target["session_key"]);
    assert_eq!(old_target["current_path"], new_target["current_path"]);
    let beta_path = f.pool.join("beta/docs/history-session/1.md");
    let before = fs::read(&beta_path).unwrap();
    let result = f.call(
        "history_session_checkpoint",
        json!({
            "session_key": old_target["session_key"],
            "expected_path": old_target["current_path"],
            "raw_user_input": "DELAYED-ALPHA-CHECKPOINT"
        }),
    );
    eprintln!("P1-HISTORY-TARGET: {result}");
    assert!(
        fs::read(&beta_path).unwrap() == before,
        "stale Alpha target silently modified Beta history"
    );
}

#[test]
#[ignore = "P1-HISTORY-BASELINE: managed metadata / Harness baseline policy requires approval"]
fn pending_history_checkpoint_must_not_block_next_project_command() {
    let f = Fixture::new();
    f.bind("alpha");
    let target = f.bootstrap("INITIAL");
    ok(f.call("start_task", json!({"objective": "project work"})));
    ok(f.call(
        "history_session_checkpoint",
        json!({
            "session_key": target["session_key"],
            "expected_path": target["current_path"],
            "raw_user_input": "save ordinary progress"
        }),
    ));
    let result = f.call("exec_command", json!({"cmd": "pwd"}));
    eprintln!("P1-HISTORY-BASELINE: {result}");
    assert_eq!(
        result["ok"], true,
        "our own History write invalidated the project task"
    );
}

#[test]
#[ignore = "P1-BINDING-LOSS: inherited FIFO/fallback semantics require approval"]
fn pending_evicted_binding_must_not_silently_read_a_different_project() {
    let f = Fixture::new();
    f.bind("alpha");
    let beta = f.pool.join("beta").canonicalize().unwrap();
    f.ctx.set_default_cwd(beta.clone());
    for index in 0..256 {
        f.ctx
            .set_session_active_project(&format!("other-chat-{index}"), beta.clone());
    }
    let state = f.call("get_active_project", json!({}));
    let result = f.call("read_file", json!({"path": "name.txt"}));
    eprintln!("P1-BINDING-LOSS state: {state}; read: {result}");
    assert!(
        result["ok"] == false || result["content"] == "alpha",
        "expired Alpha binding silently read Beta"
    );
}
