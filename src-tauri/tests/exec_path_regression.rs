mod common;

use coding_tools_mcp_desktop_lib::tools::{call_tool, wrap_mcp_tool_result, ToolContext};
use serde_json::{json, Value};
use std::path::Path;

#[cfg(windows)]
const PYTHON: &str = "python";
#[cfg(not(windows))]
const PYTHON: &str = "python3";

fn run(ctx: &ToolContext, cmd: String, cwd: &Path) -> Value {
    call_tool(
        ctx,
        "exec_command",
        &json!({"cmd": cmd, "workdir": cwd, "yield_time_ms": 10_000}),
    )
}

#[test]
fn absolute_workspace_cwd_and_interpreter_can_really_write() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project files");
    std::fs::create_dir(&project).unwrap();
    let ctx = common::ctx_for(dir.path());
    let interpreter = which::which(PYTHON).expect("test Python on PATH");
    let target = project.join("written.txt");
    // Forward slashes are accepted by Python on Windows, including paths with spaces.
    let target_arg = target.to_string_lossy().replace('\\', "/");
    let out = run(
        &ctx,
        format!(
            "\"{}\" -c \"from pathlib import Path; Path('{}').write_text('written')\"",
            interpreter.display(),
            target_arg
        ),
        &project,
    );
    assert_eq!(out["command_ok"], true, "{out}");
    assert_eq!(std::fs::read_to_string(target).unwrap(), "written");
    assert_eq!(
        out["resolved_cwd"],
        project.canonicalize().unwrap().to_string_lossy().as_ref()
    );
}

#[test]
fn absolute_outside_cwd_and_write_are_rejected_without_side_effects() {
    let parent = tempfile::tempdir().unwrap();
    let project = parent.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let ctx = common::ctx_for(&project);
    let out = run(&ctx, "pwd".into(), parent.path());
    assert_eq!(out["ok"], false, "{out}");
    let target = parent.path().join("outside.txt");
    let target_arg = target.to_string_lossy().replace('\\', "/");
    let out = run(
        &ctx,
        format!(
            "{PYTHON} -c \"from pathlib import Path; Path('{}').write_text('no')\"",
            target_arg
        ),
        &project,
    );
    assert_eq!(out["ok"], false, "{out}");
    assert!(!target.exists());
}

#[test]
fn missing_program_is_an_mcp_error_not_a_completed_operation() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = common::ctx_for(dir.path());
    let out = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "scripts/missing-python --version"}),
    );
    assert_eq!(out["transport_ok"], true, "{out}");
    assert_eq!(out["command_ok"], false);
    assert_eq!(out["ok"], false);
    assert_eq!(out["status"], "spawn_failed");
    assert!(!out["recovery_hint"]
        .as_str()
        .unwrap_or("")
        .contains("已在 standalone 模式完成"));
    let envelope = wrap_mcp_tool_result("exec_command", &json!({}), out);
    assert_eq!(envelope["isError"], true);
}

#[test]
fn nonzero_and_running_commands_do_not_receive_a_success_hint() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = common::ctx_for(dir.path());
    let out = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": format!("{PYTHON} -c \"import sys; sys.exit(7)\""),
            "yield_time_ms": 10_000
        }),
    );
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["command_ok"], false);
    assert_eq!(out["exit_code"], 7);
    assert!(!out["recovery_hint"]
        .as_str()
        .unwrap()
        .contains("已在 standalone 模式完成"));
    let out = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": format!("{PYTHON} -c \"import time; time.sleep(10)\""), "yield_time_ms": 0}),
    );
    assert_eq!(out["status"], "running", "{out}");
    assert!(out["recovery_hint"]
        .as_str()
        .unwrap()
        .contains("session_id"));
    let stopped = call_tool(
        &ctx,
        "kill_session",
        &json!({"session_id":out["session_id"],"wait_ms":2000}),
    );
    assert_eq!(stopped["ok"], true, "{stopped}");
}

#[cfg(unix)]
#[test]
fn venv_entry_keeps_its_environment_and_versioned_python_is_allowed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let bin = dir.path().join("venv/bin");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let python = which::which(PYTHON).unwrap();
    let versioned = base.join("python3.13");
    symlink(python, &versioned).unwrap();
    // A minimal pyvenv.cfg is sufficient to observe the invocation identity.
    std::fs::write(
        dir.path().join("venv/pyvenv.cfg"),
        "include-system-site-packages = false\n",
    )
    .unwrap();
    symlink(&versioned, bin.join("python")).unwrap();
    let ctx = common::ctx_for(dir.path());
    let out = run(
        &ctx,
        format!(
            "\"{}\" -c \"import sys; print(sys.prefix)\"",
            bin.join("python").display()
        ),
        dir.path(),
    );
    assert_eq!(out["command_ok"], true, "{out}");
    assert_eq!(
        Path::new(out["stdout"].as_str().unwrap().trim())
            .canonicalize()
            .unwrap(),
        dir.path().join("venv").canonicalize().unwrap()
    );
    // A native entry containing a version-like extension must not be mistaken for a script.
    let versioned_entry = dir.path().join("python3.99");
    std::fs::write(&versioned_entry, "#!/bin/sh\nprintf versioned\n").unwrap();
    std::fs::set_permissions(&versioned_entry, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = run(
        &ctx,
        format!("\"{}\"", versioned_entry.display()),
        dir.path(),
    );
    assert_eq!(out["command_ok"], true, "{out}");
    assert_eq!(out["stdout"], "versioned");
}

#[cfg(unix)]
#[test]
fn absolute_write_through_outside_symlink_is_still_rejected() {
    let parent = tempfile::tempdir().unwrap();
    let project = parent.path().join("project");
    let outside = parent.path().join("outside");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, project.join("link")).unwrap();
    let ctx = common::ctx_for(&project);
    let target = project.join("link/file.txt");
    let out = run(
        &ctx,
        format!(
            "{PYTHON} -c \"from pathlib import Path; Path('{}').write_text('no')\"",
            target.display()
        ),
        &project,
    );
    assert_eq!(out["ok"], false, "{out}");
    assert!(!outside.join("file.txt").exists());
}

#[cfg(windows)]
#[test]
fn windows_unquoted_backslashes_reach_the_process_intact() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = common::ctx_for(dir.path());
    let out = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": r#"python -c "import sys; print(sys.argv[1])" D:\some\project\file.txt"#,
            "yield_time_ms": 10_000
        }),
    );
    assert_eq!(out["command_ok"], true, "{out}");
    assert_eq!(
        out["stdout"].as_str().unwrap().trim(),
        r"D:\some\project\file.txt"
    );
}

#[test]
fn outside_executable_cannot_impersonate_an_allowlisted_name() {
    let parent = tempfile::tempdir().unwrap();
    let project = parent.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let fake_python = parent.path().join(if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    });
    std::fs::write(&fake_python, "not the allowlisted PATH executable").unwrap();
    let ctx = common::ctx_for(&project);
    let out = run(
        &ctx,
        format!("\"{}\" --version", fake_python.display()),
        &project,
    );
    assert_eq!(out["ok"], false, "{out}");
    assert_eq!(out["error"]["code"], "EXECUTABLE_OUTSIDE_WORKSPACE");
}
