//! Explicit, trusted HTTP proxy transport. Destination IPs remain policy-pinned.

use std::net::{IpAddr, SocketAddr};

use anyhow::{Context, ensure};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::egress::CONNECT_TIMEOUT;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct UpstreamConfig {
    /// Trusted HTTP proxy, e.g. http://127.0.0.1:17897. No ambient proxy inheritance.
    pub proxy: Option<String>,
    /// Optional explicitly chosen DNS JSON endpoint (HTTPS). Queries use `proxy`.
    /// Resolved IPs are checked by the normal policy before opening a tunnel.
    pub dns_over_https: Option<String>,
}

impl UpstreamConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(proxy) = &self.proxy {
            proxy_address(proxy)?;
        }
        if let Some(endpoint) = &self.dns_over_https {
            ensure!(
                self.proxy.is_some(),
                "DNS-over-HTTPS requires an explicit upstream proxy"
            );
            let url = reqwest::Url::parse(endpoint).context("invalid DNS-over-HTTPS endpoint")?;
            ensure!(
                url.scheme() == "https" && url.host_str().is_some(),
                "DNS-over-HTTPS endpoint must use HTTPS"
            );
            ensure!(
                url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "DNS-over-HTTPS endpoint must not include credentials, query or fragment"
            );
        }
        Ok(())
    }

    pub(crate) async fn connect(&self, addresses: &[SocketAddr]) -> anyhow::Result<TcpStream> {
        let proxy = proxy_address(
            self.proxy
                .as_deref()
                .context("upstream proxy is not configured")?,
        )?;
        timeout(CONNECT_TIMEOUT, async {
            let mut last_error = None;
            for target in addresses {
                match connect_tunnel(proxy, *target).await {
                    Ok(stream) => return Ok(stream),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no authorized destination address")))
        })
        .await
        .context("upstream proxy CONNECT timed out")?
    }

    pub(crate) async fn resolve(&self, host: &str, port: u16) -> anyhow::Result<Vec<SocketAddr>> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let endpoint = self
            .dns_over_https
            .as_deref()
            .context("DNS-over-HTTPS is not configured")?;
        let proxy = self
            .proxy
            .as_deref()
            .context("upstream proxy is not configured")?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(proxy)?)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CONNECT_TIMEOUT)
            .build()?;
        // Query both families; a failed query must not silently widen or bypass policy.
        let (v4, v6) = tokio::join!(
            dns_query(&client, endpoint, host, "A"),
            dns_query(&client, endpoint, host, "AAAA")
        );
        let addresses = v4?
            .into_iter()
            .chain(v6?)
            .map(|ip| SocketAddr::new(ip, port))
            .collect::<Vec<_>>();
        ensure!(
            !addresses.is_empty(),
            "DNS-over-HTTPS returned no destination addresses"
        );
        Ok(addresses)
    }
}

fn proxy_address(proxy: &str) -> anyhow::Result<SocketAddr> {
    let url = reqwest::Url::parse(proxy).context("invalid upstream proxy URL")?;
    ensure!(url.scheme() == "http", "upstream proxy must use http://");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "upstream proxy credentials are not supported"
    );
    ensure!(
        url.path() == "/" && url.query().is_none() && url.fragment().is_none(),
        "upstream proxy URL must not include a path, query or fragment"
    );
    let host = url
        .host_str()
        .context("upstream proxy requires a host")?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let ip = host
        .parse::<IpAddr>()
        .context("upstream proxy must use an explicit IP address")?;
    let port = url
        .port_or_known_default()
        .context("upstream proxy requires a port")?;
    ensure!(port != 0, "upstream proxy port must not be zero");
    Ok(SocketAddr::new(ip, port))
}

async fn connect_tunnel(proxy: SocketAddr, target: SocketAddr) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect(proxy)
        .await
        .context("connect to upstream proxy")?;
    // Send the authorized IP, never the logical hostname: the proxy must not
    // re-resolve the target and evade private-address/CIDR checks.
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        ensure!(
            headers.len() < 16 * 1024,
            "upstream CONNECT response headers too large"
        );
        // Read exactly through the header terminator; retain all tunnel bytes.
        headers.push(
            stream
                .read_u8()
                .await
                .context("read upstream CONNECT response")?,
        );
    }
    let first = std::str::from_utf8(&headers)?
        .lines()
        .next()
        .unwrap_or_default();
    let mut fields = first.split_whitespace();
    let version = fields.next().unwrap_or_default();
    let status = fields
        .next()
        .unwrap_or_default()
        .parse::<u16>()
        .unwrap_or(0);
    ensure!(
        matches!(version, "HTTP/1.0" | "HTTP/1.1") && (200..300).contains(&status),
        "upstream CONNECT failed with status {status}"
    );
    Ok(stream)
}

#[derive(Deserialize)]
struct DnsResponse {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Answer", default)]
    answers: Vec<DnsAnswer>,
}

#[derive(Deserialize)]
struct DnsAnswer {
    #[serde(rename = "type")]
    kind: u16,
    data: String,
}

async fn dns_query(
    client: &reqwest::Client,
    endpoint: &str,
    host: &str,
    kind: &str,
) -> anyhow::Result<Vec<IpAddr>> {
    let response = client
        .get(endpoint)
        .query(&[("name", host), ("type", kind)])
        .header("Accept", "application/dns-json")
        .send()
        .await?
        .error_for_status()?;
    let mut chunks = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        ensure!(
            body.len() + chunk.len() <= 64 * 1024,
            "DNS-over-HTTPS response too large"
        );
        body.extend_from_slice(&chunk);
    }
    decode_dns(&body, kind)
}

fn decode_dns(body: &[u8], kind: &str) -> anyhow::Result<Vec<IpAddr>> {
    let response: DnsResponse =
        serde_json::from_slice(body).context("invalid DNS JSON response")?;
    ensure!(
        response.status == 0,
        "DNS-over-HTTPS query failed with status {}",
        response.status
    );
    let expected = if kind == "A" { 1 } else { 28 };
    response
        .answers
        .into_iter()
        .filter(|answer| answer.kind == expected)
        .map(|answer| {
            let ip: IpAddr = answer.data.parse().context("invalid DNS address")?;
            ensure!(
                ip.is_ipv4() == (expected == 1),
                "DNS address family mismatch"
            );
            Ok(ip)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_requires_explicit_valid_transport_without_credentials() {
        assert!(proxy_address("http://127.0.0.1:17897").is_ok());
        assert!(proxy_address("http://[::1]:17897").is_ok());
        for value in [
            "socks5://127.0.0.1:1080",
            "http://u:p@127.0.0.1",
            "http://localhost:8080",
            "http://127.0.0.1/path",
            "http://127.0.0.1:0",
        ] {
            assert!(proxy_address(value).is_err(), "{value}");
        }
    }

    #[test]
    fn dns_errors_fail_closed_and_addresses_remain_available_for_policy_checks() {
        assert!(decode_dns(br#"{"Status":3}"#, "A").is_err());
        assert!(decode_dns(br#"{"Status":0,"Answer":[{"type":1,"data":"::1"}]}"#, "A").is_err());
        let addresses = decode_dns(br#"{"Status":0,"Answer":[{"type":5,"data":"alias.example"},{"type":1,"data":"127.0.0.1"}]}"#, "A").unwrap();
        assert_eq!(addresses, vec!["127.0.0.1".parse::<IpAddr>().unwrap()]);
    }

    #[tokio::test]
    async fn connect_pins_authorized_ip_and_does_not_consume_tunnel_bytes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = UpstreamConfig {
            proxy: Some(format!("http://{}", listener.local_addr().unwrap())),
            ..Default::default()
        };
        let task = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(peer.read_u8().await.unwrap());
            }
            assert_eq!(
                String::from_utf8(request).unwrap(),
                "CONNECT 203.0.113.7:443 HTTP/1.1\r\nHost: 203.0.113.7:443\r\n\r\n"
            );
            peer.write_all(b"HTTP/1.1 200 OK\r\n\r\nhello")
                .await
                .unwrap();
        });
        let mut stream = config
            .connect(&["203.0.113.7:443".parse().unwrap()])
            .await
            .unwrap();
        let mut bytes = [0; 5];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"hello");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn upstream_rejection_is_not_reported_as_a_successful_tunnel() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = UpstreamConfig {
            proxy: Some(format!("http://{}", listener.local_addr().unwrap())),
            ..Default::default()
        };
        let task = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(peer.read_u8().await.unwrap());
            }
            peer.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        assert!(
            config
                .connect(&["203.0.113.7:443".parse().unwrap()])
                .await
                .unwrap_err()
                .to_string()
                .contains("407")
        );
        task.await.unwrap();
    }
}
