use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::tools::context::ToolContext;
use crate::tools::workspace::{relative_display, tool_ok, WorkspaceError};

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "CMakeLists.txt",
    "go.mod",
    "pom.xml",
];

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
];

const MAX_SCANNED_DIRS: usize = 5000;

pub(crate) fn host_session_key(args: &Value) -> Option<&str> {
    args.get("_host_session_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn active_project_root(ctx: &ToolContext, args: &Value) -> PathBuf {
    ctx.active_project_path(host_session_key(args))
}

pub fn get_active_project(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let session_key = host_session_key(args);
    let project_root = ctx.active_project_path(session_key);
    let source = match session_key {
        Some(key) if ctx.has_session_active_project(key) => "session",
        _ if project_root != ctx.workspace.root() => "default_cwd",
        _ => "workspace",
    };
    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "active_project": relative_display(ctx.workspace.root(), &project_root),
        "project_root": project_root.display().to_string(),
        "session_scoped": session_key.is_some(),
        "source": source
    })))
}

pub fn set_active_project(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let session_key = host_session_key(args).ok_or_else(|| WorkspaceError::Tool {
        code: "SESSION_CONTEXT_REQUIRED",
        message: "Active Project is session-scoped and requires host conversation metadata.".into(),
        category: "validation",
        retryable: false,
    })?;
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let resolved = resolve_project_directory(ctx, raw)?;
    ctx.set_session_active_project(session_key, resolved.clone());
    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "active_project": relative_display(ctx.workspace.root(), &resolved),
        "project_root": resolved.display().to_string(),
        "session_scoped": true
    })))
}

pub fn discover_projects(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let max_depth = args
        .get("max_depth")
        .and_then(Value::as_u64)
        .unwrap_or(4)
        .clamp(1, 6) as usize;
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 100) as usize;

    let root = ctx.workspace.root().to_path_buf();
    let mut queue = VecDeque::from([(root.clone(), 0usize)]);
    let mut projects = Vec::new();
    let mut scanned_dirs = 0usize;
    let mut scan_truncated = false;

    while let Some((dir, depth)) = queue.pop_front() {
        if scanned_dirs >= MAX_SCANNED_DIRS {
            scan_truncated = true;
            break;
        }
        scanned_dirs += 1;

        if depth > 0 {
            let rel = relative_display(&root, &dir);
            let name = dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(&rel);
            let markers = project_markers(&dir);
            let name_match = !query.is_empty()
                && (name.to_ascii_lowercase().contains(&query)
                    || rel.to_ascii_lowercase().contains(&query));
            if (!query.is_empty() && name_match) || (query.is_empty() && !markers.is_empty()) {
                projects.push(json!({
                    "name": name,
                    "path": rel,
                    "project_root": dir.display().to_string(),
                    "markers": markers
                }));
                if projects.len() >= max_results {
                    break;
                }
            }
        }

        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP_DIRS.iter().any(|skip| name.eq_ignore_ascii_case(skip)) {
                continue;
            }
            queue.push_back((path, depth + 1));
        }
    }

    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "query": query,
        "projects": projects,
        "truncated": projects.len() >= max_results || scan_truncated,
        "scanned_dirs": scanned_dirs
    })))
}

fn resolve_project_directory(ctx: &ToolContext, raw: &str) -> Result<PathBuf, WorkspaceError> {
    if raw.contains('\0') {
        return Err(WorkspaceError::invalid_argument("Path contains a NUL byte"));
    }
    let input = Path::new(raw);
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        ctx.workspace
            .root()
            .join(raw.replace('/', std::path::MAIN_SEPARATOR_STR))
    };
    let resolved = candidate
        .canonicalize()
        .map_err(|_| WorkspaceError::not_found(format!("Path not found: {raw}")))?;
    if !resolved.starts_with(ctx.workspace.root()) {
        return Err(WorkspaceError::path_outside_workspace());
    }
    if !resolved.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "Active Project must be a directory",
        ));
    }
    Ok(resolved)
}

fn project_markers(dir: &Path) -> Vec<String> {
    let mut markers = PROJECT_MARKERS
        .iter()
        .filter(|marker| dir.join(marker).exists())
        .map(|marker| (*marker).to_string())
        .collect::<Vec<_>>();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let extension = path.extension().and_then(|value| value.to_str());
            if matches!(extension, Some(ext) if ext.eq_ignore_ascii_case("sln") || ext.eq_ignore_ascii_case("csproj"))
            {
                if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
                    markers.push(name.to_string());
                }
            }
        }
    }
    markers.sort();
    markers.dedup();
    markers
}
