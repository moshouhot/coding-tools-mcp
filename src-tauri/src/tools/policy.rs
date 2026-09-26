use std::collections::HashSet;
use std::path::{Component, Path};

use serde_json::Value;

use crate::tools::workspace::Workspace;
use crate::workspace::ActionsConfig;

use super::registry::is_actions_tool;
use super::command_line::split_command;
use super::exec_paths::{contains_external_path, resolve_workdir};

static NETWORK_COMMAND_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static DANGEROUS_COMMAND_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static INTERPRETER_MUTATION_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static CATASTROPHIC_DISK_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static RECURSIVE_DELETE_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static CURRENT_DIR_DELETE_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

const BASIC_READ_ONLY_COMMANDS: &[&str] = &[
    "pwd", "ls", "dir", "cat", "head", "tail", "grep", "find", "which", "echo",
];

const DEFAULT_ALLOWED_COMMANDS: &[&str] = &[
    "pytest",
    "python",
    "python3",
    "npm",
    "npx",
    "node",
    "pnpm",
    "yarn",
    "make",
    "mvn",
    "mvnw",
    "gradle",
    "gradlew",
    "cargo",
    "go",
    "ruff",
    "mypy",
    "eslint",
    "tsc",
    "msbuild",
    "dotnet",
    "deno",
    "bun",
    "ruby",
    "java",
    "javac",
    "cmake",
    "clang",
    "gcc",
    "g++",
    "git",
    "cmd",
    "powershell",
    "pwsh",
];

#[derive(Debug, Clone)]
pub struct PolicySettings {
    pub allowed_commands: HashSet<String>,
    pub workspace_local_entries: bool,
    pub workspace_script_extensions: HashSet<String>,
    pub max_patch_bytes: usize,
    pub permission_mode: String,
}

fn validate_trusted_shell_segments(command: &str, policy: &PolicySettings) -> Result<(), PolicyError> {
    for segment in top_level_shell_segments(command) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let parts = split_command(segment).map_err(|message| PolicyError(message.into()))?;
        let Some(executable) = parts.first() else {
            continue;
        };
        let executable = executable.trim_start_matches("./");
        let base_name = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
        let stem = base_name
            .strip_suffix(".exe")
            .or_else(|| base_name.strip_suffix(".cmd"))
            .or_else(|| base_name.strip_suffix(".bat"))
            .unwrap_or(base_name);
        if !is_allowlisted_program(policy, stem) {
            return Err(PolicyError(format!(
                "Command is not allowlisted in trusted shell segment: {stem}"
            )));
        }
    }
    Ok(())
}

fn top_level_shell_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') => {
                current.push(ch);
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                current.push(ch);
                if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => current.push(ch),
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    current.push(ch);
                } else if matches!(ch, ';' | '&' | '|') || matches!(ch, '\r' | '\n') {
                    if !current.trim().is_empty() {
                        segments.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                } else {
                    current.push(ch);
                }
            }
        }
    }
    if !current.trim().is_empty() {
        segments.push(current);
    }
    segments
}

fn catastrophic_command_reason(
    command: &str,
    arguments: &Value,
    workspace: Option<&Workspace>,
) -> Option<String> {
    let normalized = command.to_ascii_lowercase().replace('\\', "/");

    if catastrophic_disk_pattern().is_match(&normalized) {
        return Some("refusing disk format / disk wipe command".into());
    }
    if recursive_delete_targets_filesystem_root(&normalized) {
        return Some("refusing recursive deletion of a filesystem root".into());
    }

    let Some(workspace) = workspace else {
        return None;
    };
    if !recursive_delete_pattern().is_match(&normalized) {
        return None;
    }

    let workspace_root = normalize_guard_path(workspace.root());
    if !workspace_root.is_empty()
        && recursive_delete_targets_guard_path(&normalized, &workspace_root)
    {
        return Some("refusing recursive deletion of the workspace root".into());
    }

    let active_project_path = arguments
        .get("_active_project_root")
        .and_then(Value::as_str)
        .and_then(|raw| workspace.resolve_existing(raw).ok())
        .map(|resolved| resolved.path);
    if let Some(project_root) = active_project_path.as_deref() {
        let project_guard = normalize_guard_path(project_root);
        if project_root != workspace.root()
            && !project_guard.is_empty()
            && recursive_delete_targets_guard_path(&normalized, &project_guard)
        {
            return Some("refusing recursive deletion of the active project root".into());
        }
    }

    #[cfg(windows)]
    if let Ok(system_root) = std::env::var("SystemRoot") {
        let system_root = normalize_guard_path(Path::new(&system_root));
        if !system_root.is_empty()
            && recursive_delete_targets_guard_path(&normalized, &system_root)
        {
            return Some("refusing recursive deletion of the Windows system directory".into());
        }
    }
    if ["%systemroot%", "%windir%", "$env:systemroot", "$env:windir"]
        .iter()
        .any(|guard| recursive_delete_targets_guard_path(&normalized, guard))
    {
        return Some("refusing recursive deletion of the Windows system directory".into());
    }

    let workdir = arguments
        .get("workdir")
        .or_else(|| arguments.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let resolved_workdir = resolve_workdir(workspace, workdir)
        .ok()
        .and_then(|resolved| resolved.path.canonicalize().ok());
    let at_workspace_root = resolved_workdir
        .as_ref()
        .zip(workspace.root().canonicalize().ok().as_ref())
        .is_some_and(|(cwd, root)| cwd == root);
    if at_workspace_root && current_dir_delete_pattern().is_match(&normalized) {
        return Some("refusing recursive deletion of the workspace root".into());
    }
    let at_active_project_root = resolved_workdir
        .as_ref()
        .zip(active_project_path.as_ref())
        .is_some_and(|(cwd, root)| cwd == root);
    if at_active_project_root && current_dir_delete_pattern().is_match(&normalized) {
        return Some("refusing recursive deletion of the active project root".into());
    }

    None
}

fn normalize_guard_path(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('\\', "/");
    normalized
        .strip_prefix("//?/")
        .unwrap_or(&normalized)
        .trim_end_matches('/')
        .to_string()
}

fn catastrophic_disk_pattern() -> &'static regex::Regex {
    CATASTROPHIC_DISK_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(^|[;&|]\s*)(format(?:\.com)?\s+[a-z]:|mkfs(?:\.[a-z0-9_-]+)?\b|diskpart\b[^\r\n]*\bclean(?:\s+all)?\b|clear-disk\b|format-volume\b)",
        )
        .expect("valid regex")
    })
}

fn recursive_delete_pattern() -> &'static regex::Regex {
    RECURSIVE_DELETE_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(\brm\s+(?:-[^\s]*r[^\s]*\b|--recursive\b)|\bremove-item\b[^\r\n]*-recurse\b|\b(?:rmdir|rd)\s+/s\b|\b(?:del|erase)\s+/s\b|shutil\.rmtree\s*\()",
        )
        .expect("valid regex")
    })
}

fn recursive_delete_targets_filesystem_root(command: &str) -> bool {
    let root_target = regex::Regex::new(
        r#"(?i)(?:^|[\s'\"(=,])(?:/|[a-z]:/+)(?:\*)?(?:$|[\s'\"),])"#,
    )
    .expect("valid root target regex");
    command
        .split([';', '&', '|', '\r', '\n'])
        .any(|segment| {
            recursive_delete_pattern().is_match(segment) && root_target.is_match(segment)
        })
}

fn recursive_delete_targets_guard_path(command: &str, guard: &str) -> bool {
    if guard.is_empty() {
        return false;
    }
    let escaped = regex::escape(guard.trim_end_matches('/'));
    let target = format!(
        r#"(?:^|[\s'\"(=,]){escaped}(?:/+\*?)?(?:$|[\s'\"),])"#
    );
    let Ok(target_pattern) = regex::Regex::new(&target) else {
        return false;
    };

    command
        .split([';', '&', '|', '\r', '\n'])
        .any(|segment| {
            recursive_delete_pattern().is_match(segment) && target_pattern.is_match(segment)
        })
}

fn current_dir_delete_pattern() -> &'static regex::Regex {
    CURRENT_DIR_DELETE_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(\brm\s+(?:-[^\s]*r[^\s]*|--recursive)\s+(?:\.|\./|\.\/\*|\*)\s*$|\bremove-item\b[^\r\n]*(?:\s\.\s|\s\*\s)[^\r\n]*-recurse\b|\bremove-item\b[^\r\n]*-recurse[^\r\n]*(?:\s\.\s*$|\s\*\s*$)|\b(?:rmdir|rd)\s+/s\b[^\r\n]*\s\.\s*$|\bdel\s+/s\b[^\r\n]*\s\*\s*$|shutil\.rmtree\s*\(\s*['\"]\.['\"]\s*\))"#,
        )
        .expect("valid regex")
    })
}

impl Default for PolicySettings {
    fn default() -> Self {
        Self {
            allowed_commands: default_allowed_command_set(),
            workspace_local_entries: true,
            workspace_script_extensions: default_workspace_script_extension_set(),
            max_patch_bytes: 200_000,
            // Internal/test fallback remains conservative. New persisted workspaces
            // default to dangerous via workspace::default_permission_mode().
            permission_mode: "trusted".into(),
        }
    }
}

impl PolicySettings {
    pub fn from_runtime(runtime: &crate::workspace::RuntimeConfig) -> Self {
        Self {
            allowed_commands: merge_default_allowed_commands(&runtime.allowed_commands),
            workspace_local_entries: runtime.workspace_local_entries,
            workspace_script_extensions: parse_workspace_script_extensions(
                &runtime.workspace_script_extensions,
            ),
            max_patch_bytes: 200_000,
            permission_mode: runtime.permission_mode.clone(),
        }
    }

    pub fn from_actions_config(actions: &ActionsConfig) -> Self {
        Self {
            allowed_commands: merge_default_allowed_commands(&actions.allowed_commands),
            workspace_local_entries: true,
            workspace_script_extensions: default_workspace_script_extension_set(),
            max_patch_bytes: actions.max_patch_bytes as usize,
            permission_mode: actions.permission_mode.clone(),
        }
    }

    pub fn network_allowed(&self) -> bool {
        self.permission_mode == "trusted" || self.permission_mode == "dangerous"
    }

    pub fn skip_permission_gates(&self) -> bool {
        self.permission_mode == "dangerous"
    }

    pub fn shell_syntax_allowed(&self) -> bool {
        self.permission_mode == "trusted" || self.permission_mode == "dangerous"
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PolicyError(pub String);

pub fn parse_allowed_commands(configured: &str) -> HashSet<String> {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return default_allowed_command_set();
    }
    let mut commands: HashSet<String> = trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    // 基础诊断命令是工作区可用性的最低保障，不应因 Actions 配置遗漏而失效。
    commands.extend(BASIC_READ_ONLY_COMMANDS.iter().map(|s| s.to_string()));
    commands
}

pub fn parse_workspace_script_extensions(configured: &str) -> HashSet<String> {
    let mut extensions = configured
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with('.') {
                value.to_ascii_lowercase()
            } else {
                format!(".{}", value.to_ascii_lowercase())
            }
        })
        .collect::<HashSet<_>>();
    if extensions.is_empty() {
        extensions = default_workspace_script_extension_set();
    }
    extensions
}

fn default_allowed_command_set() -> HashSet<String> {
    DEFAULT_ALLOWED_COMMANDS
        .iter()
        .map(|s| s.to_string())
        .chain(BASIC_READ_ONLY_COMMANDS.iter().map(|s| s.to_string()))
        .collect()
}

fn merge_default_allowed_commands(configured: &str) -> HashSet<String> {
    let mut commands = default_allowed_command_set();
    commands.extend(parse_allowed_commands(configured));
    commands
}

fn default_workspace_script_extension_set() -> HashSet<String> {
    [".exe", ".bat", ".cmd", ".ps1"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn validate_tool_arguments(
    tool_name: &str,
    arguments: &Value,
    policy: &PolicySettings,
) -> Result<(), PolicyError> {
    validate_tool_arguments_for_workspace(tool_name, arguments, policy, None)
}

pub fn validate_tool_arguments_for_workspace(
    tool_name: &str,
    arguments: &Value,
    policy: &PolicySettings,
    workspace: Option<&Workspace>,
) -> Result<(), PolicyError> {
    match tool_name {
        "exec_command" => validate_command_for_workspace(arguments, policy, workspace),
        "apply_patch" | "patch_check" => validate_patch(arguments, policy),
        _ => Ok(()),
    }
}

/// Actions OpenAPI 暴露层校验：仅限制「能否调用」，不参与执行逻辑。
pub fn validate_actions_exposure(tool_name: &str) -> Result<(), PolicyError> {
    if is_actions_tool(tool_name) {
        Ok(())
    } else {
        Err(PolicyError(format!("Tool is not exposed: {tool_name}")))
    }
}

pub fn validate_command(arguments: &Value, policy: &PolicySettings) -> Result<(), PolicyError> {
    validate_command_for_workspace(arguments, policy, None)
}

pub fn validate_command_for_workspace(
    arguments: &Value,
    policy: &PolicySettings,
    workspace: Option<&Workspace>,
) -> Result<(), PolicyError> {
    let command = arguments
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| PolicyError("exec_command requires a non-empty cmd".into()))?;
    if command.trim().is_empty() {
        return Err(PolicyError("exec_command requires a non-empty cmd".into()));
    }
    if !policy.skip_permission_gates() && command.len() > 4_000 {
        return Err(PolicyError("Command is too long".into()));
    }
    let filesystem_scope = arguments
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace");
    if filesystem_scope != "workspace" {
        return Err(PolicyError(
            "EXTERNAL_EXECUTION_NOT_ALLOWED: exec_command 只允许在 Workspace 内执行".into(),
        ));
    }
    for key in ["workdir", "cwd"] {
        if let Some(workdir) = arguments.get(key).and_then(Value::as_str) {
            let path = Path::new(workdir);
            if path.components().any(|part| part == Component::ParentDir)
                || (path.is_absolute() && workspace.is_none()) {
                return Err(PolicyError(
                    "workdir must stay inside the configured workspace".into(),
                ));
            }
            if let Some(workspace) = workspace {
                resolve_workdir(workspace, workdir).map_err(|e| PolicyError(e.message()))?;
            }
        }
    }
    let parts = split_command(command).map_err(|message| PolicyError(message.into()))?;
    if parts.is_empty() { return Err(PolicyError("Empty command".into())); }
    if let Some(reason) = catastrophic_command_reason(command, arguments, workspace) {
        return Err(PolicyError(format!("CATASTROPHIC_OPERATION_BLOCKED: {reason}")));
    }
    if command_requires_shell(command) && !policy.shell_syntax_allowed() {
        return Err(PolicyError(
            "Shell chaining, redirection and expansion are blocked in safe permission mode".into(),
        ));
    }
    if command_requires_shell(command) && policy.permission_mode == "trusted" {
        validate_trusted_shell_segments(command, policy)?;
    }
    if !policy.skip_permission_gates()
        && (dangerous_command_pattern().is_match(command)
            || interpreter_mutation_pattern().is_match(command))
        && command_targets_protected_repository_asset(command)
    {
        return Err(PolicyError(
            "PROTECTED_REPOSITORY_ASSET: 禁止删除或递归清空 .git/.github".into(),
        ));
    }
    if !policy.skip_permission_gates()
        && interpreter_mutation_pattern().is_match(command)
        && contains_external_path(&parts[1..], workspace)
    {
        return Err(PolicyError(
            "WORKSPACE_PATH_PROTECTED: workspace scope 禁止通过子进程写入 Workspace 外部路径"
                .into(),
        ));
    }
    if !policy.skip_permission_gates()
        && dangerous_command_pattern().is_match(command)
        && !arguments
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err(PolicyError(
            "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION: dangerous command requires confirm=true"
                .into(),
        ));
    }
    if !policy.skip_permission_gates()
        && network_command_pattern().is_match(command)
        && !policy.network_allowed()
    {
        return Err(PolicyError(
            "Network-looking commands are blocked in safe permission mode".into(),
        ));
    }

    let executable = parts[0].trim_start_matches("./");
    let base_name = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
    let stem = base_name
        .strip_suffix(".exe")
        .or_else(|| base_name.strip_suffix(".cmd"))
        .or_else(|| base_name.strip_suffix(".bat"))
        .unwrap_or(base_name);

    let workspace_entry_candidate = workspace_local_entry_exists(workspace, arguments, executable)
        || executable.contains(['/', '\\'])
        || policy
            .workspace_script_extensions
            .iter()
            .any(|extension| base_name.to_ascii_lowercase().ends_with(extension));
    if !policy.skip_permission_gates()
        && !(is_allowlisted_program(policy, stem)
            || (policy.workspace_local_entries && workspace_entry_candidate))
    {
        return Err(PolicyError(format!("Command is not allowlisted: {stem}")));
    }

    if !policy.skip_permission_gates() && arguments.get("env").is_some() {
        return Err(PolicyError(
            "Environment variables cannot be supplied by GPT".into(),
        ));
    }

    if let Some(timeout_ms) = arguments.get("timeout_ms").and_then(Value::as_u64) {
        if !policy.skip_permission_gates() && timeout_ms > 600_000 {
            return Err(PolicyError("Command timeout exceeds 10 minutes".into()));
        }
    }

    Ok(())
}

pub(super) fn is_allowlisted_program(policy: &PolicySettings, name: &str) -> bool {
    let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".cmd"))
        .or_else(|| name.strip_suffix(".bat")).unwrap_or(name);
    policy.allowed_commands.contains(name) || name.strip_prefix("python3.").is_some_and(|version| {
        !version.is_empty() && version.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            && policy.allowed_commands.contains("python3")
    })
}

fn workspace_local_entry_exists(
    workspace: Option<&Workspace>,
    arguments: &Value,
    executable: &str,
) -> bool {
    let Some(workspace) = workspace else {
        return false;
    };
    let workdir = arguments
        .get("workdir")
        .or_else(|| arguments.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let Ok(base) = resolve_workdir(workspace, workdir) else {
        return false;
    };
    let candidate = if Path::new(executable).is_absolute() {
        Path::new(executable).to_path_buf()
    } else {
        base.path.join(executable)
    };
    candidate
        .canonicalize()
        .map(|path| path.is_file() && path.starts_with(workspace.root()))
        .unwrap_or(false)
}

pub fn validate_patch(arguments: &Value, policy: &PolicySettings) -> Result<(), PolicyError> {
    let patch = arguments
        .get("patch")
        .and_then(Value::as_str)
        .ok_or_else(|| PolicyError("apply_patch requires a patch".into()))?;
    if patch.trim().is_empty() {
        return Err(PolicyError("apply_patch requires a patch".into()));
    }

    if patch.len() > policy.max_patch_bytes {
        return Err(PolicyError("Patch is too large".into()));
    }

    Ok(())
}

pub(super) fn command_requires_shell(command: &str) -> bool {
    if command.contains(['\r', '\n']) {
        return true;
    }

    let chars: Vec<char> = command.chars().collect();
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }

        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => {}
            None => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else if matches!(ch, ';' | '&' | '|' | '>' | '<' | '`')
                    || (ch == '$'
                        && chars
                            .get(index + 1)
                            .is_some_and(|next| *next == '(' || *next == '{'))
                {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn network_command_pattern() -> &'static regex::Regex {
    NETWORK_COMMAND_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(https?://|urllib\.request|requests\.|http\.client|\bcurl\b|\bwget\b|\bssh\b|\bscp\b|\bftp\b)",
        )
        .expect("valid regex")
    })
}

fn dangerous_command_pattern() -> &'static regex::Regex {
    DANGEROUS_COMMAND_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(git\s+reset\s+--hard|git\s+clean\s+-[^\r\n]*f|git\s+checkout\s+--\s+\.|(^|\s)rm\s+(-[^\r\n]*r[^\r\n]*f|--recursive)|remove-item\s+[^\r\n]*-recurse|(^|\s)(rmdir|del)\s+/s\b)",
        )
        .expect("valid regex")
    })
}

fn interpreter_mutation_pattern() -> &'static regex::Regex {
    INTERPRETER_MUTATION_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(shutil\.(rmtree|move)|os\.(remove|unlink|rmdir)|pathlib\.[^\s;]+\.(unlink|rename)|write_text|write_bytes|fs\.(writefile|writefilesync|unlink|rm)|set-content|out-file|new-item|files?\.(write|delete)|open\([^)]*['\"]w)"#,
        )
        .expect("valid regex")
    })
}


fn command_targets_protected_repository_asset(command: &str) -> bool {
    let normalized_command = command.to_ascii_lowercase().replace('\\', "/");
    let references_protected_asset =
        normalized_command.contains(".git") || normalized_command.contains(".github");
    if !references_protected_asset {
        return false;
    }

    let mutating_operation = [
        "rm ",
        "remove-item",
        "rmdir",
        "del ",
        "unlink",
        "rmtree",
        "write_text",
        "writefile",
        "rename",
        "move",
        "checkout",
        "clean ",
    ]
    .iter()
    .any(|needle| normalized_command.contains(needle));
    if mutating_operation {
        return true;
    }

    command.split_whitespace().any(|part| {
        let token = part
            .trim_matches(|ch: char| matches!(ch, '\'' | '"' | '`' | ',' | ';'))
            .replace('\\', "/");
        let token = token.strip_prefix("./").unwrap_or(&token);
        token == ".git"
            || token.starts_with(".git/")
            || token == ".github"
            || token.starts_with(".github/")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspace_allowed_commands_override_defaults() {
        let actions = ActionsConfig {
            allowed_commands: "cargo,go".into(),
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        assert!(policy.allowed_commands.contains("cargo"));
        assert!(policy.allowed_commands.contains("pytest"));
    }

    #[test]
    fn trusted_mode_accepts_any_configured_workspace_script_extension() {
        let policy = PolicySettings {
            workspace_local_entries: true,
            workspace_script_extensions: parse_workspace_script_extensions(".cmd,.launcher"),
            ..PolicySettings::default()
        };
        assert!(
            validate_command(&serde_json::json!({ "cmd": "anything.launcher" }), &policy).is_ok()
        );
        assert!(validate_command(
            &serde_json::json!({ "cmd": "scripts/another-name.cmd" }),
            &policy
        )
        .is_ok());
    }

    #[test]
    fn trusted_mode_accepts_an_extensionless_workspace_entry() {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::write(dir.path().join("project-entry"), "#!/bin/sh\necho ok\n").expect("entry");
        let workspace = Workspace::new(dir.path().to_path_buf()).expect("workspace");
        assert!(validate_command_for_workspace(
            &serde_json::json!({ "cmd": "project-entry", "workdir": "." }),
            &PolicySettings::default(),
            Some(&workspace),
        )
        .is_ok());
    }

    #[test]
    fn patch_size_uses_workspace_limit() {
        let actions = ActionsConfig {
            max_patch_bytes: 10,
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        let err = validate_patch(&json!({ "patch": "01234567890" }), &policy).unwrap_err();
        assert!(err.0.contains("too large"));
    }

    #[test]
    fn basic_diagnostic_commands_are_allowed() {
        let policy = PolicySettings::default();
        for command in BASIC_READ_ONLY_COMMANDS {
            validate_command(&json!({"cmd": command}), &policy)
                .unwrap_or_else(|err| panic!("{command} should be allowed: {err}"));
        }
    }

    #[test]
    fn configured_commands_keep_basic_diagnostics() {
        let actions = ActionsConfig {
            allowed_commands: "cargo,go".into(),
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        assert!(validate_command(&json!({"cmd": "pwd"}), &policy).is_ok());
        assert!(validate_command(&json!({"cmd": "pytest"}), &policy).is_ok());
    }

    #[test]
    fn trusted_allows_shell_syntax_while_safe_still_blocks_it() {
        let policy = PolicySettings::default();
        assert!(validate_command(
            &json!({"cmd": "python -c \"import os; print(os.getcwd())\""}),
            &policy
        )
        .is_ok());
        assert!(validate_command(
            &json!({"cmd": "python -c \"print(1)\" && echo nope"}),
            &policy
        )
        .is_ok());
        assert!(validate_command(&json!({"cmd": "echo hello > output.txt"}), &policy).is_ok());

        let safe = PolicySettings {
            permission_mode: "safe".into(),
            ..PolicySettings::default()
        };
        assert!(validate_command(
            &json!({"cmd": "python -c \"print(1)\" && echo nope"}),
            &safe
        )
        .is_err());
        assert!(validate_command(&json!({"cmd": "echo hello > output.txt"}), &safe).is_err());

        let bypass = validate_command(
            &json!({"cmd": "echo allowed && certutil -hashfile README.md SHA256"}),
            &policy,
        )
        .expect_err("trusted shell chaining must not bypass the executable allowlist");
        assert!(bypass.0.contains("certutil"), "{bypass}");
    }

    #[test]
    fn dangerous_mode_bypasses_normal_exec_gates() {
        let policy = PolicySettings {
            permission_mode: "dangerous".into(),
            ..PolicySettings::default()
        };
        for args in [
            json!({"cmd": "certutil -hashfile README.md SHA256"}),
            json!({"cmd": "git reset --hard HEAD"}),
            json!({"cmd": "echo first && where.exe git"}),
            json!({"cmd": "python -c \"print('inline')\"", "env": {"LOCAL_TEST_FLAG": "1"}}),
            json!({"cmd": "python --version", "timeout_ms": 3_600_000_u64}),
        ] {
            validate_command(&args, &policy)
                .unwrap_or_else(|err| panic!("dangerous mode should accept {args}: {err}"));
        }
    }

    #[test]
    fn dangerous_mode_keeps_catastrophic_safety_floor() {
        let workspace_dir = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(workspace_dir.path().to_path_buf()).expect("workspace");
        let policy = PolicySettings {
            permission_mode: "dangerous".into(),
            ..PolicySettings::default()
        };

        for command in [
            "format C:",
            "mkfs.ext4 /dev/sda",
            "rm -rf /",
            "rm -r /",
            "powershell -Command \"Remove-Item $env:SystemRoot -Recurse -Force\"",
        ] {
            let err = validate_command_for_workspace(
                &json!({"cmd": command, "workdir": "."}),
                &policy,
                Some(&workspace),
            )
            .expect_err("catastrophic command must stay blocked");
            assert!(err.0.contains("CATASTROPHIC_OPERATION_BLOCKED"), "{err}");
        }

        for command in ["rm -rf .", "rm -r .", "rm --recursive ."] {
            let err = validate_command_for_workspace(
                &json!({"cmd": command, "workdir": "."}),
                &policy,
                Some(&workspace),
            )
            .expect_err("workspace root deletion must stay blocked");
            assert!(err.0.contains("workspace root"), "{err}");
        }

        let workspace_root = normalize_guard_path(workspace.root());
        let err = validate_command_for_workspace(
            &json!({
                "cmd": format!("rm -rf \"{workspace_root}\""),
                "workdir": "."
            }),
            &policy,
            Some(&workspace),
        )
        .expect_err("absolute workspace root deletion must stay blocked");
        assert!(err.0.contains("workspace root"), "{err}");

        assert!(validate_command_for_workspace(
            &json!({
                "cmd": format!("rm -rf \"{workspace_root}/target\""),
                "workdir": "."
            }),
            &policy,
            Some(&workspace),
        )
        .is_ok());

        assert!(validate_command_for_workspace(
            &json!({"cmd": "powershell -Command \"Remove-Item target -Recurse -Force\"", "workdir": "."}),
            &policy,
            Some(&workspace),
        )
        .is_ok());
    }
}
