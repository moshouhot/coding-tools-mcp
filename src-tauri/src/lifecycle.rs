//! 应用生命周期日志。
//!
//! 目的：让"上次为什么没了"可追溯。此前只在内存/控制台打日志，
//! 装机后 stderr 不可见，崩溃或服务意外退出后没有任何证据留存。
//!
//! 约束：
//! - **持久化**到用户配置目录，按大小轮转并限制份数。
//! - **脱敏**：不得写入隧道 token、完整命令行、cookie、密钥。
//! - 写失败**不影响**主流程（日志是旁路，不能让程序因为日志挂掉）。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::AppResult;
use crate::platform::platform;

/// 单文件上限 1 MiB，超过即轮转。
const MAX_BYTES: u64 = 1024 * 1024;
/// 保留份数（含当前文件）：`lifecycle.log` + `.1` .. `.3`
const KEEP_FILES: usize = 4;
const FILE_NAME: &str = "lifecycle.log";

static LOG_LOCK: Mutex<()> = Mutex::new(());

/// 日志目录：`<app_config_dir>/logs/app/`。
fn log_dir() -> AppResult<PathBuf> {
    Ok(platform().app_config_dir()?.join("logs").join("app"))
}

/// 写一行生命周期日志。
///
/// 任何失败都只返回 `Err` 给调用方记录，**不 panic、不阻塞**。
pub fn write_line(line: &str) -> AppResult<()> {
    let dir = log_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(FILE_NAME);

    let _guard = LOG_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    rotate_if_needed(&path)?;

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{} {}", now_stamp(), sanitize(line))?;
    Ok(())
}

/// 写日志并忽略错误。用于"必须记录但绝不能影响主流程"的位置。
pub fn record(line: &str) {
    if let Err(error) = write_line(line) {
        eprintln!("[lifecycle] 写日志失败: {error}");
    }
}

fn now_stamp() -> String {
    // 只用秒级 UTC 时间戳，避免引入时间格式化依赖。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("[{secs}]")
}

fn rotate_if_needed(path: &Path) -> AppResult<()> {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < MAX_BYTES {
        return Ok(());
    }

    // .2 -> .3, .1 -> .2, 当前 -> .1；最旧一份丢弃。
    for index in (1..KEEP_FILES).rev() {
        let from = suffix_path(path, index);
        if from.exists() {
            let to = suffix_path(path, index + 1);
            if index + 1 >= KEEP_FILES {
                let _ = fs::remove_file(&from);
            } else {
                let _ = fs::rename(&from, &to);
            }
        }
    }
    // 当前文件改名为 .1
    if path.exists() {
        let _ = fs::rename(path, suffix_path(path, 1));
    }
    Ok(())
}

fn suffix_path(base: &Path, index: usize) -> PathBuf {
    let mut name = base.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{index}"));
    base.with_file_name(name)
}

/// 脱敏：抹掉 token / 密钥 / 完整命令行里可能夹带的东西。
///
/// 宁可过度抹除，也不要把凭据落到磁盘上。
pub fn sanitize(input: &str) -> String {
    let mut out = input.to_string();

    // tunnel token / 通用 token 值：`--token xxx`、`token=xxx`、`token: xxx`
    for marker in ["--token", "token=", "token:", "TOKEN="] {
        out = redact_after_marker(&out, marker);
    }
    // Cloudflare 隧道 token 是 base64 JSON，常以 eyJ 开头。
    out = redact_eyj(&out);
    // 常见密钥字段。
    for marker in ["secret=", "apikey=", "api_key=", "password=", "Authorization:"] {
        out = redact_after_marker(&out, marker);
    }
    out
}

fn redact_after_marker(input: &str, marker: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find(marker) {
        result.push_str(&rest[..pos]);
        result.push_str(marker);
        result.push_str("[REDACTED]");
        // 先跳过标记与值之间的空白，**再**从值的第一个字符开始找边界。
        //
        // 早前版本直接对 `after` 找空白，而 `after` 以空格开头，
        // `cut` 恒为 0，导致值被原样留下（`--token aaa` 里的 aaa 泄露）。
        let after = rest[pos + marker.len()..].trim_start();
        let cut = after
            .find(value_boundary)
            .unwrap_or(after.len());
        let tail = &after[cut..];
        if tail.is_empty() {
            rest = tail;
        } else {
            result.push(' ');
            rest = tail.trim_start();
        }
    }
    result.push_str(rest);
    result
}

fn redact_eyj(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find("eyJ") {
        result.push_str(&rest[..pos]);
        result.push_str("[REDACTED]");
        let after = &rest[pos..];
        let cut = after.find(value_boundary).unwrap_or(after.len());
        let tail = &after[cut..];
        if tail.is_empty() {
            rest = tail;
        } else {
            result.push(' ');
            rest = tail.trim_start();
        }
    }
    result.push_str(rest);
    result
}

/// 值结束的判定字符：空白或常见字段分隔符。
fn value_boundary(c: char) -> bool {
    c.is_whitespace() || c == ',' || c == '"' || c == '\''
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_token_flag_values() {
        let line = "启动 cloudflared: tunnel run --token eyJhIjoiZDYxZGE2MmQ5,tail";
        let out = sanitize(line);
        assert!(!out.contains("eyJhIjoiZDYxZGE2MmQ5"), "token 必须被抹掉: {out}");
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_bare_eyj_blob() {
        let out = sanitize("payload=eyJhIjoixyz next=1");
        assert!(!out.contains("eyJhIjoixyz"));
        assert!(out.contains("next=1"), "不应吞掉后续正常内容: {out}");
    }

    #[test]
    fn redacts_key_value_secrets() {
        let out = sanitize("secret=abc123 retry=3");
        assert!(!out.contains("abc123"));
        assert!(out.contains("retry=3"));
    }

    #[test]
    fn keeps_ordinary_lifecycle_text_intact() {
        let line = "服务 MCP 启动成功 pid=1234 port=28766";
        assert_eq!(sanitize(line), line);
    }

    #[test]
    fn redaction_is_stable_with_multiple_occurrences() {
        let out = sanitize("--token aaa --token bbb");
        assert!(!out.contains("aaa"));
        assert!(!out.contains("bbb"));
    }
}
