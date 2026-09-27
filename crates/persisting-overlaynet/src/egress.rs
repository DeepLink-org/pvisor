//! VM egress authorization, resolution, and rate limiting.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::bandwidth::{BandwidthRegistry, BandwidthSession};
use crate::policy::{DenyReason, NetworkPolicy};
use crate::resolver::{
    ResolvedAddressPolicy, TargetAuthorizationError, authorize_target_with_policy,
};
use persisting_control::{
    AttemptId, ControlController, NetworkAccessRequest, NetworkTransport, RunId, StorylineId,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct EgressContext {
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
    pub storyline_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum EgressError {
    #[error("egress denied: {0:?}")]
    Denied(DenyReason),
    #[error("egress resolution failed: {0:#}")]
    Resolve(anyhow::Error),
    #[error("egress connection to {host}:{port} timed out")]
    ConnectTimeout { host: String, port: u16 },
    #[error("egress connection to {host}:{port} failed: {source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },
}

/// Attempt-scoped authorization services for transparent VM flows.
///
/// The compiled policy remains an invariant and an injected controller may
/// only narrow it. The injected registry shares identical bandwidth buckets
/// with the explicit proxy running in the same Attempt.
#[derive(Clone)]
pub struct EgressRuntime {
    policy: NetworkPolicy,
    controller: Arc<dyn ControlController>,
    bandwidth: BandwidthRegistry,
}

impl std::fmt::Debug for EgressRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressRuntime")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl EgressRuntime {
    pub fn with_bandwidth_registry(
        policy: NetworkPolicy,
        controller: Arc<dyn ControlController>,
        bandwidth: BandwidthRegistry,
    ) -> Self {
        Self {
            policy,
            controller,
            bandwidth,
        }
    }

    pub(crate) async fn authorize_tcp(
        &self,
        context: &EgressContext,
        host: &str,
        port: u16,
    ) -> Result<(Vec<SocketAddr>, BandwidthSession), EgressError> {
        let request = NetworkAccessRequest {
            run_id: context.run_id.clone().map(RunId),
            attempt_id: context.attempt_id.clone().map(AttemptId),
            storyline_id: context.storyline_id.clone().map(StorylineId),
            host: host.to_owned(),
            port: Some(port),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: None,
        };
        let authorized = authorize_target_with_policy(
            self.controller.as_ref(),
            &self.policy,
            request,
            ResolvedAddressPolicy::HostConnectorAliases,
        )
        .await
        .map_err(|error| match error {
            TargetAuthorizationError::Denied(reason) => EgressError::Denied(reason),
            TargetAuthorizationError::Resolve(error) => EgressError::Resolve(error),
        })?;
        let bandwidth = self
            .bandwidth
            .session(
                self.policy
                    .matching_limits(host, Some(port), &authorized.addresses),
            )
            .await;
        Ok((authorized.addresses, bandwidth))
    }
}

pub(crate) async fn connect_tcp_addresses(
    addresses: &[SocketAddr],
    host: &str,
    port: u16,
) -> Result<TcpStream, EgressError> {
    let mut last_error = None;
    timeout(CONNECT_TIMEOUT, async {
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "no authorized address",
            )
        }))
    })
    .await
    .map_err(|_| EgressError::ConnectTimeout {
        host: host.to_owned(),
        port,
    })?
    .map_err(|source| EgressError::Connect {
        host: host.to_owned(),
        port,
        source,
    })
}

/// Preserve the host's configured HTTP proxy as the upstream route for
/// pVisor-managed TCP tunnels. The guest still connects to the logical target
/// and policy is checked against that target before this helper is called.
pub(crate) async fn connect_via_ambient_http_proxy(
    host: &str,
    port: u16,
) -> Option<std::io::Result<TcpStream>> {
    // A host proxy cannot reach this machine's loopback service reliably, and
    // may acknowledge CONNECT before it has connected to the destination.
    if is_loopback_destination(host) {
        return None;
    }
    let proxy = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| value.starts_with("http://") && !is_pvisor_loopback_proxy(value))?;
    Some(connect_via_http_proxy(&proxy, host, port).await)
}

fn is_loopback_destination(host: &str) -> bool {
    host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn is_pvisor_loopback_proxy(value: &str) -> bool {
    let Some(authority) = value.strip_prefix("http://") else {
        return false;
    };
    let authority = authority.split('/').next().unwrap_or(authority);
    authority.starts_with("127.0.0.1:492")
        || authority.starts_with("127.0.0.1:493")
        || authority.starts_with("[::1]:492")
        || authority.starts_with("[::1]:493")
}

async fn connect_via_http_proxy(proxy: &str, host: &str, port: u16) -> std::io::Result<TcpStream> {
    let endpoint = proxy
        .strip_prefix("http://")
        .unwrap_or(proxy)
        .split('/')
        .next()
        .unwrap_or(proxy);
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    timeout(CONNECT_TIMEOUT, async {
        let mut stream = TcpStream::connect(endpoint).await?;
        let request = format!(
            "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: Keep-Alive\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        while response.len() < 16 * 1024 {
            stream.read_exact(&mut byte).await?;
            response.push(byte[0]);
            if response.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&response);
        let status = head.lines().next().unwrap_or_default();
        if status.split_whitespace().nth(1) != Some("200") {
            return Err(std::io::Error::other(format!(
                "upstream proxy CONNECT {authority} failed: {status}"
            )));
        }
        Ok(stream)
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "upstream proxy timeout"))?
}

#[cfg(test)]
mod tests {
    use super::is_loopback_destination;

    #[test]
    fn loopback_destinations_bypass_the_host_proxy() {
        for host in ["127.0.0.1", "127.0.0.2", "::1", "localhost", "LOCALHOST."] {
            assert!(is_loopback_destination(host), "{host}");
        }
        for host in ["127.0.0.1.example.com", "192.168.1.1", "example.com"] {
            assert!(!is_loopback_destination(host), "{host}");
        }
    }
}
