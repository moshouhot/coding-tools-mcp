//! Cloudflare tunnel 在线判据。
//!
//! 关键原则：**「进程存在」不等于「隧道在线」**。
//! 实测故障里 cloudflared 活着、本地 MCP 正常，但 `/ready` 返回
//! `503 + readyConnections:0`，公网 530 —— 只看 pid 会把这种情况误判为运行中。
//!
//! 因此这里用 cloudflared 自己的 metrics 端点判定：
//! - `/ready` HTTP 200 且 `readyConnections > 0` → 在线
//! - 其余（连不上 / 非 200 / 0 连接）→ 离线
//!
//! 端口必须由启动参数固定（`--metrics`），不能依赖 cloudflared 的默认探测序列
//! `[20241..20245]`，否则多实例会互相抢端口。

use std::time::Duration;

/// 单个工作区隧道使用的固定 metrics 端口。
///
/// 从 20241 起分配，避开 cloudflared 自己的默认候选序列起点冲突：
/// 我们用显式 `--metrics 127.0.0.1:<port>`，所以只要互不重复即可。
const METRICS_PORT_BASE: u16 = 20251;

/// `/ready` 探测超时。必须短于健康检查周期，避免堆积。
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 隧道在线状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelReadiness {
    /// `/ready` 200 且至少一条 edge 连接。
    Ready { connections: i64 },
    /// 进程在，但连接未建立（cloudflared 仍在退避重连）。
    NotReady { detail: String },
    /// 连不上 metrics 端点：可能是进程已退、或端口未监听。
    Unreachable { detail: String },
}

impl TunnelReadiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, TunnelReadiness::Ready { .. })
    }
}

/// 为工作区分配稳定的 metrics 端口。
///
/// 用 workspace id 的稳定哈希取模，保证同一工作区每次启动端口一致
/// （健康检查要按端口探测，端口漂移会导致探测落空）。
pub fn metrics_port_for(workspace_id: &str) -> u16 {
    // FNV-1a：稳定、无额外依赖，且不受 HashMap 随机种子影响。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in workspace_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // 限制在 20251..20603，避开系统常用端口区间。
    METRICS_PORT_BASE + (hash % 352) as u16
}

/// 解析 `/ready` 响应体。
///
/// cloudflared 返回形如：
/// `{"status":503,"readyConnections":0,"connectorId":"..."}`
///
/// 不引入 JSON 依赖的完整反序列化：字段极少，且要容忍未来新增字段。
/// 解析失败时按 NotReady 处理 —— **不能**把无法判读当成在线。
pub fn parse_ready_response(status_code: u16, body: &str) -> TunnelReadiness {
    let connections = extract_i64_field(body, "readyConnections");
    let Some(connections) = connections else {
        return TunnelReadiness::NotReady {
            detail: format!("/ready 响应无法解析 readyConnections: {body}"),
        };
    };

    if status_code == 200 && connections > 0 {
        TunnelReadiness::Ready { connections }
    } else {
        TunnelReadiness::NotReady {
            detail: format!("http={status_code} readyConnections={connections}"),
        }
    }
}

/// 从极简 JSON 片段中取一个整数字段。
///
/// 只支持本场景所需的扁平结构；解析不出来返回 `None` 由调用方判为未就绪。
fn extract_i64_field(body: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\"");
    let start = body.find(&needle)? + needle.len();
    let rest = body[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse::<i64>().ok()
}

/// 探测指定 metrics 端口是否表示隧道在线。
///
/// 任何网络/解析错误都折叠为 `Unreachable`（带原因），
/// 由调用方决定是"标记离线"还是"触发恢复"。
pub async fn probe_readiness(metrics_port: u16) -> TunnelReadiness {
    let url = format!("http://127.0.0.1:{metrics_port}/ready");
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(client) => client,
        Err(err) => {
            return TunnelReadiness::Unreachable {
                detail: format!("构建探测客户端失败: {err}"),
            }
        }
    };

    match client.get(&url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            match response.text().await {
                Ok(body) => parse_ready_response(status, &body),
                Err(err) => TunnelReadiness::Unreachable {
                    detail: format!("读取 /ready 响应失败: {err}"),
                },
            }
        }
        Err(err) => TunnelReadiness::Unreachable {
            detail: format!("连接 metrics 端口 {metrics_port} 失败: {err}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_response_with_connections_is_online() {
        let verdict = parse_ready_response(
            200,
            r#"{"status":200,"readyConnections":4,"connectorId":"abc"}"#,
        );
        assert_eq!(verdict, TunnelReadiness::Ready { connections: 4 });
        assert!(verdict.is_ready());
    }

    #[test]
    fn zero_connections_is_not_ready_even_with_http_200() {
        // 实测故障形态：进程活着但 readyConnections=0。
        // 若这里判成在线，就会把"隧道断了"显示成"运行中"。
        let verdict = parse_ready_response(
            200,
            r#"{"status":0,"readyConnections":0,"connectorId":"abc"}"#,
        );
        assert!(!verdict.is_ready(), "0 连接必须判为未就绪");
    }

    #[test]
    fn service_unavailable_with_zero_connections_is_not_ready() {
        // 现场实际返回值：503 + readyConnections:0
        let verdict = parse_ready_response(
            503,
            r#"{"status":503,"readyConnections":0,"connectorId":"68a726b1"}"#,
        );
        assert_eq!(
            verdict,
            TunnelReadiness::NotReady {
                detail: "http=503 readyConnections=0".into()
            }
        );
    }

    #[test]
    fn unparsable_body_is_not_ready_not_online() {
        // 无法判读时不能乐观当成在线。
        let verdict = parse_ready_response(200, "<html>gateway</html>");
        assert!(!verdict.is_ready());
        assert!(matches!(verdict, TunnelReadiness::NotReady { .. }));
    }

    #[test]
    fn ready_with_connections_but_non_200_is_not_ready() {
        let verdict = parse_ready_response(
            503,
            r#"{"status":503,"readyConnections":2}"#,
        );
        assert!(!verdict.is_ready(), "非 200 不能仅仅因为连接数>0 就判在线");
    }

    #[test]
    fn metrics_port_is_stable_per_workspace() {
        let a = metrics_port_for("3a6d1aa715274e03bb23747a6d187ef7");
        let b = metrics_port_for("3a6d1aa715274e03bb23747a6d187ef7");
        assert_eq!(a, b, "同一工作区端口必须稳定");
    }

    #[test]
    fn metrics_port_differs_between_workspaces() {
        let a = metrics_port_for("workspace-a");
        let b = metrics_port_for("workspace-b");
        assert_ne!(a, b, "不同工作区必须用不同端口，避免互抢");
    }

    /// 隔离故障注入：本地起一个只返回固定响应的 TCP 服务，
    /// 验证 probe_readiness 的 HTTP 路径与判定，不触碰任何现用服务。
    ///
    /// 循环 accept：reqwest 可能先建立探测连接，单次 accept 会被它消耗掉。
    async fn probe_against_stub(status_line: &str, body: &'static str) -> TunnelReadiness {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let status_line = status_line.to_string();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let response = format!(
                    "HTTP/1.1 {status_line}
Content-Type: application/json
Content-Length: {}
Connection: close

{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
                // 给对端读完的时间，再关闭。
                tokio::time::sleep(Duration::from_millis(30)).await;
                let _ = socket.shutdown().await;
            }
        });
        probe_readiness(port).await
    }

    #[tokio::test]
    async fn probe_marks_503_zero_connections_as_not_ready() {
        let verdict = probe_against_stub(
            "503 Service Unavailable",
            r#"{"status":503,"readyConnections":0}"#,
        )
        .await;
        assert!(!verdict.is_ready(), "503/0 必须判离线，实际 {:?}", verdict);
        assert!(matches!(verdict, TunnelReadiness::NotReady { .. }));
    }

    #[tokio::test]
    async fn probe_marks_200_with_connections_as_ready() {
        let verdict = probe_against_stub(
            "200 OK",
            r#"{"status":200,"readyConnections":4}"#,
        )
        .await;
        assert_eq!(verdict, TunnelReadiness::Ready { connections: 4 });
    }

    #[tokio::test]
    async fn probe_marks_200_zero_connections_as_not_ready() {
        // 最容易骗过"进程存在即在线"那种判定的形态。
        let verdict = probe_against_stub("200 OK", r#"{"status":0,"readyConnections":0}"#).await;
        assert!(!verdict.is_ready(), "200 但 0 连接仍必须判离线");
    }

    #[tokio::test]
    async fn probe_reports_unreachable_when_nothing_listens() {
        // 用一个几乎不可能被监听的端口：探测必须折叠为 Unreachable，
        // 而不是 panic 或误报在线。
        let verdict = probe_readiness(1).await;
        assert!(matches!(verdict, TunnelReadiness::Unreachable { .. }), "{verdict:?}");
    }

    #[test]
    fn metrics_port_stays_in_reserved_range() {
        for id in ["a", "b", "", "3a6d1aa715274e03bb23747a6d187ef7", "z".repeat(64).as_str()] {
            let port = metrics_port_for(id);
            assert!(
                (METRICS_PORT_BASE..METRICS_PORT_BASE + 352).contains(&port),
                "端口 {port} 越界"
            );
        }
    }
}
