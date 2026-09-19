//! 托盘状态的采样与聚合。
//!
//! 采样必须反映**实际**监督状态：运行中要看 MCP 监听器是否真的在跑，
//! 隧道是否在线要看隧道监督器里对应会话/进程是否存活，而不是只看保存过的公网 URL。
//! 聚合结果只有三档，颜色之外还有文字说明，避免只靠颜色传达状态。

use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

use super::icon::Level;

/// 单个工作区 MCP 服务与隧道的采样结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSample {
    /// 已停止。
    Stopped,
    /// 正在启动或停止。
    Transitioning,
    /// 运行中且（若配置了隧道）隧道监督状态正常。
    Healthy,
    /// 运行中但隧道未连上。
    TunnelOffline,
    /// MCP 自身处于错误态（未能监听端口 / 启动失败）。
    McpError,
}

/// 一个工作区的采样。
#[derive(Debug, Clone)]
pub struct WorkspaceSample {
    /// 工作区名称（tooltip 与自启文案使用）。
    #[allow(dead_code)]
    pub name: String,
    pub mcp: ServiceSample,
    /// 该工作区是否配置了隧道（决定“隧道未连”是否算异常）。
    pub tunnel_configured: bool,
    /// 隧道是否确实在线。
    pub tunnel_online: bool,
}

/// 托盘总状态。
#[derive(Debug, Clone)]
pub struct TrayState {
    pub level: Level,
    pub tooltip: String,
    pub summary: String,
    /// 是否有任何 MCP 正在运行。测试与外部复用。
    #[allow(dead_code)]
    pub any_running: bool,
    /// 各工作区采样明细。测试与外部复用。
    #[allow(dead_code)]
    pub workspaces: Vec<WorkspaceSample>,
}

/// 由单个工作区采样推导其服务档位。
pub fn classify(sample: &WorkspaceSample) -> ServiceSample {
    match sample.mcp {
        // MCP 自身的错误与「MCP 正常但隧道离线」必须区分，不能都算运行中。
        ServiceSample::McpError => ServiceSample::McpError,
        ServiceSample::Transitioning => ServiceSample::Transitioning,
        ServiceSample::Stopped => ServiceSample::Stopped,
        ServiceSample::Healthy | ServiceSample::TunnelOffline => {
            if sample.tunnel_configured && !sample.tunnel_online {
                ServiceSample::TunnelOffline
            } else {
                sample.mcp
            }
        }
    }
}

/// 聚合多个工作区采样为托盘总状态。
///
/// - 全部停止 → 灰色
/// - 存在启动中/停止中、MCP 错误，或「运行中但隧道未连」 → 黄色
/// - 至少一个运行且全部健康 → 绿色
pub fn aggregate(samples: Vec<WorkspaceSample>) -> TrayState {
    let classified: Vec<ServiceSample> = samples.iter().map(classify).collect();

    // 只有 MCP 真正监听中的工作区才算「运行」；MCP 错误不算运行。
    let is_running = |s: &ServiceSample| {
        matches!(s, ServiceSample::Healthy | ServiceSample::TunnelOffline)
    };
    let any_running = classified.iter().any(is_running);
    let any_transitioning = classified
        .iter()
        .any(|s| matches!(s, ServiceSample::Transitioning));
    let any_tunnel_offline = classified
        .iter()
        .any(|s| matches!(s, ServiceSample::TunnelOffline));
    let any_mcp_error = classified
        .iter()
        .any(|s| matches!(s, ServiceSample::McpError));

    let level = if any_transitioning || any_tunnel_offline || any_mcp_error {
        Level::Warn
    } else if any_running {
        Level::Ok
    } else {
        Level::Stopped
    };

    let running_count = classified.iter().filter(|s| is_running(s)).count();
    let total = samples.len();

    let summary = if total == 0 {
        "尚未添加工作区".to_string()
    } else if any_mcp_error {
        format!("MCP 启动失败（{running_count}/{total} 运行）")
    } else if any_transitioning {
        format!("MCP 启动中（{running_count}/{total} 运行）")
    } else if any_tunnel_offline {
        format!("隧道未连接（{running_count}/{total} 运行）")
    } else if any_running {
        format!("MCP 运行中（{running_count}/{total}）")
    } else {
        format!("MCP 已停止（0/{total}）")
    };

    let tooltip = if total == 0 {
        "Coding Tools MCP · 尚未添加工作区".to_string()
    } else {
        // tooltip 同时给出 MCP 与隧道细节，避免只能靠颜色判断；工作区过多时截断。
        const MAX_LINES: usize = 5;
        let mut lines = vec![format!("Coding Tools MCP · {summary}")];
        for sample in samples.iter().take(MAX_LINES) {
            lines.push(format!("・{}：{}", sample.name, describe(sample)));
        }
        if samples.len() > MAX_LINES {
            lines.push(format!("・…另有 {} 个工作区", samples.len() - MAX_LINES));
        }
        lines.join("\n")
    };

    TrayState {
        level,
        tooltip,
        summary,
        any_running,
        workspaces: samples,
    }
}

/// 单个工作区的可读描述（tooltip 用）。
///
/// 注意：这里的「正常」只表示**本机隧道进程/会话监督状态正常**，
/// 不等同于公网端到端可达（后者需要外部探测）。
pub fn describe(sample: &WorkspaceSample) -> String {
    match classify(sample) {
        ServiceSample::Healthy if sample.tunnel_configured => {
            "运行中 · 隧道进程正常".to_string()
        }
        ServiceSample::Healthy => "运行中".to_string(),
        ServiceSample::TunnelOffline => "运行中 · 隧道未连接".to_string(),
        ServiceSample::McpError => "MCP 启动失败".to_string(),
        ServiceSample::Transitioning => "启动/停止中".to_string(),
        ServiceSample::Stopped => "已停止".to_string(),
    }
}

/// 该工作区是否配置了 MCP 隧道。
pub fn mcp_tunnel_configured(profile: &WorkspaceProfile) -> bool {
    let kind = profile.tunnel.tunnel_type.trim();
    !kind.is_empty() && kind != "none"
}

/// 供命令层复用的默认设置读取。
pub fn load_settings() -> AppSettings {
    AppSettings::load_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(mcp: ServiceSample, configured: bool, online: bool) -> WorkspaceSample {
        WorkspaceSample {
            name: "w".into(),
            mcp,
            tunnel_configured: configured,
            tunnel_online: online,
        }
    }

    #[test]
    fn all_stopped_is_gray() {
        let state = aggregate(vec![
            sample(ServiceSample::Stopped, true, false),
            sample(ServiceSample::Stopped, false, false),
        ]);
        assert_eq!(state.level, Level::Stopped);
        assert!(!state.any_running);
        assert!(state.summary.contains("已停止"));
    }

    #[test]
    fn no_workspaces_is_gray_and_explained() {
        let state = aggregate(vec![]);
        assert_eq!(state.level, Level::Stopped);
        assert!(state.summary.contains("尚未添加"));
        assert!(state.tooltip.contains("尚未添加"));
    }

    #[test]
    fn running_without_tunnel_is_green() {
        let state = aggregate(vec![sample(ServiceSample::Healthy, false, false)]);
        assert_eq!(state.level, Level::Ok);
        assert!(state.any_running);
    }

    #[test]
    fn running_with_healthy_tunnel_is_green() {
        let state = aggregate(vec![sample(ServiceSample::Healthy, true, true)]);
        assert_eq!(state.level, Level::Ok);
    }

    #[test]
    fn running_with_dead_tunnel_is_yellow() {
        let state = aggregate(vec![sample(ServiceSample::Healthy, true, false)]);
        assert_eq!(state.level, Level::Warn);
        assert!(state.any_running, "MCP 仍在运行，只是隧道未连");
        assert!(state.summary.contains("隧道未连接"));
    }

    #[test]
    fn mcp_error_is_yellow_and_not_counted_as_running() {
        // MCP 启动失败绝不能报成「运行中」，也不能描述为隧道问题。
        let state = aggregate(vec![sample(ServiceSample::McpError, true, false)]);
        assert_eq!(state.level, Level::Warn);
        assert!(!state.any_running, "MCP 错误不能算作运行中");
        assert!(state.summary.contains("MCP 启动失败"), "{}", state.summary);
        assert!(!state.summary.contains("隧道未连接"));
        assert!(state.tooltip.contains("MCP 启动失败"));
    }

    #[test]
    fn mcp_error_takes_precedence_over_tunnel_message() {
        let state = aggregate(vec![
            sample(ServiceSample::McpError, true, false),
            sample(ServiceSample::Healthy, true, false),
        ]);
        assert_eq!(state.level, Level::Warn);
        assert!(state.summary.contains("MCP 启动失败"));
        assert!(state.summary.contains("1/2"), "{}", state.summary);
    }

    #[test]
    fn starting_is_yellow_even_when_others_run() {
        let state = aggregate(vec![
            sample(ServiceSample::Healthy, true, true),
            sample(ServiceSample::Transitioning, true, false),
        ]);
        assert_eq!(state.level, Level::Warn);
        assert!(state.summary.contains("启动中"));
    }

    #[test]
    fn one_green_one_gray_is_still_green() {
        // 多工作区场景：只要有一个健康运行，总状态就是绿色。
        let state = aggregate(vec![
            sample(ServiceSample::Healthy, true, true),
            sample(ServiceSample::Stopped, true, false),
        ]);
        assert_eq!(state.level, Level::Ok);
        assert!(state.summary.contains("1/2"));
    }

    #[test]
    fn degraded_beats_healthy_for_level_but_reports_running() {
        let state = aggregate(vec![
            sample(ServiceSample::Healthy, true, true),
            sample(ServiceSample::Healthy, true, false),
        ]);
        assert_eq!(state.level, Level::Warn);
        assert_eq!(
            state
                .workspaces
                .iter()
                .filter(|w| matches!(
                    classify(w),
                    ServiceSample::Healthy | ServiceSample::TunnelOffline
                ))
                .count(),
            2
        );
    }
}
