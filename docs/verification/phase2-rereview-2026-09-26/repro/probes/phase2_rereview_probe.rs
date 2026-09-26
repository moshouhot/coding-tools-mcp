//! TEMPORARY independent advisor re-review probes (2026-09-26 Phase 2 re-review).
//!
//! This file is NOT part of the product test suite. It is created only to
//! produce deterministic RED/GREEN evidence for the advisor report and is
//! removed again after evidence capture.
//!
//! Convention:
//!   * `safe_*` tests assert the SECURE expectation. A failure is a confirmed
//!     product defect (RED repro), not a product PASS.
//!   * `vuln_confirmation_*` tests intentionally assert the VULNERABLE behavior
//!     so the defect is deterministic. They are evidence only, never an
//!     acceptance gate.

use std::fs;
use std::path::{Path, PathBuf};

use coding_tools_mcp_desktop_lib::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

struct Fixture {
    ctx: ToolContext,
    pool: PathBuf,
    harness: PathBuf,
    _temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self::with_projects(&["alpha", "beta"])
    }

    fn with_projects(projects: &[&str]) -> Self {
        let temp = tempfile::tempdir().expect("isolated fixture");
        let pool = temp.path().join("pool");
        let harness = temp.path().join("harness");
        fs::create_dir_all(&pool).unwrap();
        for project in projects {
            fs::create_dir_all(pool.join(project)).unwrap();
            fs::write(pool.join(project).join("name.txt"), *project).unwrap();
        }
        let ctx = dangerous_context(&pool, &harness);
        Self {
            ctx,
            pool,
            harness,
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

fn dangerous_context(pool: &Path, harness: &Path) -> ToolContext {
    let mut ctx = ToolContext::for_test(pool.to_path_buf(), harness.to_path_buf())
        .expect("tool context");
    ctx.permission_mode = "dangerous".into();
    ctx.policy.permission_mode = "dangerous".into();
    ctx
}

fn ok(value: Value) -> Value {
    assert_eq!(value["ok"], true, "{value}");
    value
}

fn state_file(harness: &Path) -> PathBuf {
    let dir = harness.join("active-project-sessions");
    fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .expect("active-project state file")
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

// ---------------------------------------------------------------------------
// H1: corrupt binding store + repair one session, then recreate context again.
// ---------------------------------------------------------------------------
#[test]
fn safe_lost_binding_does_not_silently_fall_back_after_repair_restart() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    for project in ["alpha", "beta"] {
        fs::create_dir_all(pool.join(project)).unwrap();
        fs::write(pool.join(project).join("name.txt"), project).unwrap();
    }

    // Bind an old conversation to alpha and persist it.
    {
        let ctx = dangerous_context(&pool, &harness);
        ok(call_tool(
            &ctx,
            "set_active_project",
            &json!({"path": "alpha", "_host_session_key": "old-chat"}),
        ));
    }

    // Corrupt the durable store.
    fs::write(state_file(&harness), b"{ not valid json").unwrap();

    // First recreation: the store is detected as corrupt -> fail closed.
    let ctx2 = dangerous_context(&pool, &harness);
    let closed = call_tool(
        &ctx2,
        "get_active_project",
        &json!({"_host_session_key": "old-chat"}),
    );
    assert_eq!(closed["ok"], false, "corrupt store must fail closed: {closed}");
    assert_eq!(
        closed["error"]["code"],
        "ACTIVE_PROJECT_STORE_UNAVAILABLE"
    );

    // Repair by rebinding ONE other conversation. This rewrites the store
    // containing only the repaired session.
    ok(call_tool(
        &ctx2,
        "set_active_project",
        &json!({"path": "beta", "_host_session_key": "repair-chat"}),
    ));

    // Second recreation: the store now loads fine, but `old-chat` is absent.
    let ctx3 = dangerous_context(&pool, &harness);
    ctx3.set_default_cwd(pool.join("beta").canonicalize().unwrap());
    let lost = call_tool(
        &ctx3,
        "get_active_project",
        &json!({"_host_session_key": "old-chat"}),
    );

    // SECURE expectation: a previously established binding must never silently
    // fall back to default_cwd just because the store was repaired for another
    // session. It should stay fail-closed until old-chat explicitly rebinds.
    assert_eq!(
        lost["ok"], false,
        "lost session silently fell back to default_cwd after repair restart: {lost}"
    );
    assert_eq!(
        lost["error"]["code"],
        "ACTIVE_PROJECT_STORE_UNAVAILABLE",
        "{lost}"
    );
}

#[test]
fn vuln_confirmation_lost_session_reads_default_project_after_repair_restart() {
    // Explicit vulnerability confirmation (evidence only, not an acceptance
    // gate): demonstrates that the lost session actually reads beta.
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    for project in ["alpha", "beta"] {
        fs::create_dir_all(pool.join(project)).unwrap();
        fs::write(pool.join(project).join("name.txt"), project).unwrap();
    }
    {
        let ctx = dangerous_context(&pool, &harness);
        ok(call_tool(
            &ctx,
            "set_active_project",
            &json!({"path": "alpha", "_host_session_key": "old-chat"}),
        ));
    }
    fs::write(state_file(&harness), b"{ not valid json").unwrap();
    let ctx2 = dangerous_context(&pool, &harness);
    ok(call_tool(
        &ctx2,
        "set_active_project",
        &json!({"path": "beta", "_host_session_key": "repair-chat"}),
    ));
    let ctx3 = dangerous_context(&pool, &harness);
    ctx3.set_default_cwd(pool.join("beta").canonicalize().unwrap());
    let lost = ok(call_tool(
        &ctx3,
        "get_active_project",
        &json!({"_host_session_key": "old-chat"}),
    ));
    assert_eq!(lost["source"], "default_cwd", "{lost}");
    assert_eq!(lost["active_project"], "beta", "{lost}");
}

// ---------------------------------------------------------------------------
// H2: two long-lived contexts sharing one store clobber each other.
// ---------------------------------------------------------------------------
#[test]
fn safe_two_contexts_keep_each_others_durable_bindings() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    for project in ["alpha", "beta"] {
        fs::create_dir_all(pool.join(project)).unwrap();
    }
    let alpha = pool.join("alpha").canonicalize().unwrap();
    let beta = pool.join("beta").canonicalize().unwrap();

    // Two long-lived ToolContext instances over the SAME workspace/harness store.
    let ctx_a = dangerous_context(&pool, &harness);
    let ctx_b = dangerous_context(&pool, &harness);

    ctx_a.set_session_active_project("s1", alpha).unwrap();
    ctx_b.set_session_active_project("s2", beta).unwrap();

    // A fresh context must observe BOTH durable bindings.
    let ctx_c = dangerous_context(&pool, &harness);
    assert!(
        ctx_c.has_session_active_project("s1"),
        "binding s1 was clobbered by a second context's sequential write"
    );
    assert!(
        ctx_c.has_session_active_project("s2"),
        "binding s2 missing"
    );
}

// ---------------------------------------------------------------------------
// H3a: read-only History call with history_dir="src" exempts real code.
// ---------------------------------------------------------------------------
#[test]
fn safe_readonly_history_dir_must_not_exempt_real_code_from_baseline() {
    let f = Fixture::new();
    fs::create_dir_all(f.pool.join("alpha/src")).unwrap();
    fs::write(f.pool.join("alpha/src/code.rs"), "fn real() {}\n").unwrap();
    f.bind("alpha");

    // READ-ONLY History call pointing at a real source directory. It must not
    // register that directory as a Harness-ignored path.
    let search = f.call(
        "history_session_search",
        json!({"history_dir": "src", "query": "nothing"}),
    );
    assert_eq!(search["ok"], true, "{search}");

    ok(f.call("start_task", json!({"objective": "baseline"})));

    // External change to real code AFTER the baseline must be blocked.
    fs::write(f.pool.join("alpha/src/code.rs"), "fn real() { changed(); }\n").unwrap();
    let result = f.call("exec_command", json!({"cmd": "pwd"}));
    assert_eq!(
        result["ok"], false,
        "real code under a history_dir was exempted from the Harness baseline: {result}"
    );
    assert_eq!(result["error"]["code"], "FILE_CHANGED_EXTERNALLY", "{result}");
}

// ---------------------------------------------------------------------------
// H3b: read-only History calls depend on writable metadata.
// ---------------------------------------------------------------------------
#[test]
fn safe_readonly_history_search_works_with_unwritable_metadata_store() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("pool");
    let harness = temp.path().join("harness");
    fs::create_dir_all(pool.join("alpha")).unwrap();
    // Make the managed-path store path unusable (a file where a dir is needed).
    fs::create_dir_all(&harness).unwrap();
    fs::write(harness.join("managed-project-paths"), b"blocked").unwrap();

    let ctx = dangerous_context(&pool, &harness);
    ok(call_tool(
        &ctx,
        "set_active_project",
        &json!({"path": "alpha", "_host_session_key": "review-chat"}),
    ));
    let search = call_tool(
        &ctx,
        "history_session_search",
        &json!({"query": "", "_host_session_key": "review-chat"}),
    );
    // SECURE expectation: a read-only search must not require writing metadata.
    assert_eq!(
        search["ok"], true,
        "read-only history search failed because metadata was unwritable: {search}"
    );
}

// ---------------------------------------------------------------------------
// H4: Active Project == workspace root drops custom history exclusions.
// ---------------------------------------------------------------------------
#[test]
fn safe_root_active_project_keeps_custom_history_exclusion() {
    let f = Fixture::new();
    // Bind the workspace root itself as the Active Project.
    ok(f.call("set_active_project", json!({"path": "."})));

    let target = ok(f.call(
        "history_session_bootstrap",
        json!({
            "initial_user_input": "INITIAL",
            "history_dir": ".ai/session-history"
        }),
    ));
    ok(f.call("start_task", json!({"objective": "project work"})));
    ok(f.call(
        "history_session_checkpoint",
        json!({
            "session_key": target["session_key"],
            "expected_path": target["current_path"],
            "project_id": target["project_id"],
            "history_dir": ".ai/session-history",
            "raw_user_input": "save custom history"
        }),
    ));

    let result = f.call("exec_command", json!({"cmd": "pwd"}));
    assert_eq!(
        result["ok"], true,
        "custom History write invalidated the root-project baseline: {result}"
    );
}

// ---------------------------------------------------------------------------
// H5: bound directory replaced by an in-pool symlink silently retargets.
// ---------------------------------------------------------------------------
#[test]
fn safe_bound_project_replaced_by_inpool_symlink_fails_closed() {
    let f = Fixture::new();
    f.bind("alpha");
    fs::remove_dir_all(f.pool.join("alpha")).unwrap();
    let target = f.pool.join("beta").canonicalize().unwrap();
    if make_dir_symlink(&target, &f.pool.join("alpha")).is_err() {
        eprintln!("UNTESTED: OS refused to create a directory symlink");
        return;
    }

    let result = f.call("read_file", json!({"path": "name.txt"}));
    // SECURE expectation: the bound directory is gone; do not silently read
    // another project through a replaced symlink.
    assert_eq!(
        result["ok"], false,
        "conversation silently retargeted to another project via symlink: {result}"
    );
    assert_eq!(result["error"]["code"], "ACTIVE_PROJECT_UNAVAILABLE", "{result}");
}
