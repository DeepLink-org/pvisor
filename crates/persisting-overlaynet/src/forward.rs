//! Explicit HTTP proxy forwarding: CONNECT tunnel and absolute-URI requests.

use axum::body::Body;
use axum::extract::Request;
use axum::http::uri::Authority;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::bandwidth::BandwidthSession;
use crate::egress::{CONNECT_TIMEOUT, connect_tcp_addresses, connect_via_ambient_http_proxy};
use crate::headers::skip_transparent_forward_header_for;
use crate::resolver::AuthorizedTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectTarget {
    pub host: String,
    pub port: u16,
    pub authority: String,
}

pub(crate) async fn handle_connect_authorized(
    req: Request,
    target: ConnectTarget,
    authorized: &AuthorizedTarget,
    bandwidth: BandwidthSession,
) -> anyhow::Result<Response> {
    // HTTP/1 CONNECT uses hyper's connection upgrade. HTTP/2 has no
    // connection-wide upgrade; extended CONNECT carries the tunnel in the
    // request and response bodies, so it must be bridged as a duplex stream.
    if req.version() == axum::http::Version::HTTP_2 {
        let (_parts, body) = req.into_parts();
        let destination = match connect_via_ambient_http_proxy(&target.host, target.port).await {
            Some(Ok(stream)) => stream,
            Some(Err(error)) => {
                return Err(anyhow::anyhow!(
                    "CONNECT to {} through the host proxy failed: {error}",
                    target.authority
                ));
            }
            None => connect_tcp_addresses(&authorized.addresses, &target.host, target.port)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("CONNECT to {} failed: {error}", target.authority)
                })?,
        };
        let (destination_read, mut destination_write) = destination.into_split();
        let mut upload = body.into_data_stream();
        let upload_bandwidth = bandwidth.clone();
        tokio::spawn(async move {
            while let Some(chunk) = upload.next().await {
                let Ok(chunk) = chunk else { break };
                upload_bandwidth.throttle(chunk.len()).await;
                if destination_write.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            let _ = destination_write.shutdown().await;
        });
        let stream = futures_util::stream::unfold(
            (destination_read, bandwidth),
            |(mut reader, bandwidth)| async move {
                let mut buffer = vec![0_u8; 16 * 1024];
                match reader.read(&mut buffer).await {
                    Ok(0) => None,
                    Ok(read) => {
                        bandwidth.throttle(read).await;
                        Some((
                            Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buffer[..read])),
                            (reader, bandwidth),
                        ))
                    }
                    // A read error terminates the tunnel. Returning the same
                    // reader here would make `Stream` yield the error forever.
                    Err(_) => None,
                }
            },
        );
        return Response::builder()
            .status(StatusCode::OK)
            .body(Body::from_stream(stream))
            .map_err(|error| anyhow::anyhow!("build HTTP/2 CONNECT response: {error}"))
            .map(IntoResponse::into_response);
    }
    let on_upgrade: OnUpgrade = hyper::upgrade::on(req);
    // Establish the upstream before reporting success. Returning 200 first
    // makes a refused or unroutable destination look like an accepted tunnel.
    let mut destination = match connect_via_ambient_http_proxy(&target.host, target.port).await {
        Some(Ok(stream)) => stream,
        Some(Err(error)) => {
            return Err(anyhow::anyhow!(
                "CONNECT to {} through the host proxy failed: {error}",
                target.authority
            ));
        }
        None => connect_tcp_addresses(&authorized.addresses, &target.host, target.port)
            .await
            .map_err(|error| anyhow::anyhow!("CONNECT to {} failed: {error}", target.authority))?,
    };
    tokio::spawn(async move {
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let client = TokioIo::new(upgraded);
        let (client_read, client_write) = tokio::io::split(client);
        let (destination_read, destination_write) = destination.split();
        let upload = copy_limited(client_read, destination_write, bandwidth.clone());
        let download = copy_limited(destination_read, client_write, bandwidth);
        let _ = tokio::join!(upload, download);
    });
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::empty())
        .expect("CONNECT response")
        .into_response())
}

pub(crate) fn parse_connect_target(authority: &str) -> anyhow::Result<ConnectTarget> {
    if authority.contains('@') {
        anyhow::bail!("CONNECT authority must not include user information");
    }
    let parsed = authority
        .parse::<Authority>()
        .map_err(|error| anyhow::anyhow!("invalid CONNECT authority `{authority}`: {error}"))?;
    let parsed_host = parsed.host();
    let host = parsed_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(parsed_host);
    if host.is_empty() {
        anyhow::bail!("CONNECT authority must include a host");
    }
    let explicit_port = if authority.starts_with('[') {
        authority
            .find(']')
            .and_then(|end| authority[end + 1..].strip_prefix(':'))
    } else {
        authority.rsplit_once(':').map(|(_, port)| port)
    };
    let port = explicit_port
        .map(|port| {
            port.parse::<u16>()
                .map_err(|_| anyhow::anyhow!("CONNECT authority port is out of range"))
        })
        .transpose()?
        .unwrap_or(443);
    if port == 0 {
        anyhow::bail!("CONNECT authority port must not be zero");
    }
    let normalized_authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    Ok(ConnectTarget {
        host: host.to_string(),
        port,
        authority: normalized_authority,
    })
}

pub fn is_forward_proxy_request(method: &Method, uri: &Uri) -> bool {
    method == Method::CONNECT || uri.scheme().is_some()
}

pub(crate) async fn transparent_forward_authorized(
    req: Request,
    target: &AuthorizedTarget,
    bandwidth: BandwidthSession,
) -> anyhow::Result<Response<Body>> {
    let (parts, body) = req.into_parts();
    let url = parts.uri.to_string();

    let mut client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(Duration::from_secs(600));
    if let Some(proxy) = ambient_upstream_proxy() {
        client = client.proxy(reqwest::Proxy::all(&proxy).map_err(|error| {
            anyhow::anyhow!("configure ambient upstream proxy {proxy}: {error}")
        })?);
    }
    if target.host.parse::<std::net::IpAddr>().is_err() {
        client = client.resolve_to_addrs(&target.host, &target.addresses);
    }
    let client = client
        .build()
        .map_err(|error| anyhow::anyhow!("build pinned forward client: {error}"))?;
    let mut request = client.request(parts.method, url);
    for (name, value) in &parts.headers {
        if !skip_transparent_forward_header_for(&parts.headers, name.as_str()) {
            request = request.header(name, value);
        }
    }
    let upload_bandwidth = bandwidth.clone();
    let upload = body.into_data_stream().then(move |result| {
        let bandwidth = upload_bandwidth.clone();
        async move {
            if let Ok(bytes) = &result {
                bandwidth.throttle(bytes.len()).await;
            }
            result
        }
    });
    let response = request
        .body(reqwest::Body::wrap_stream(upload))
        .send()
        .await
        .map_err(|error| anyhow::anyhow!("forward request: {error}"))?;
    let status = response.status();
    let headers = response.headers().clone();

    let mut builder = Response::builder().status(status);
    for (name, value) in &headers {
        if !skip_transparent_forward_header_for(&headers, name.as_str()) {
            builder = builder.header(name, value);
        }
    }
    let download = response.bytes_stream().then(move |result| {
        let bandwidth = bandwidth.clone();
        async move {
            if let Ok(bytes) = &result {
                bandwidth.throttle(bytes.len()).await;
            }
            result
        }
    });
    builder
        .body(Body::from_stream(download))
        .map_err(|error| anyhow::anyhow!("build response: {error}"))
}

fn ambient_upstream_proxy() -> Option<String> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok())
    .find(|value| {
        !value.is_empty() && !value.contains("127.0.0.1:492") && !value.contains("127.0.0.1:493")
    })
}

async fn copy_limited<R, W>(
    mut reader: R,
    mut writer: W,
    bandwidth: BandwidthSession,
) -> std::io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            writer.shutdown().await?;
            return Ok(copied);
        }
        bandwidth.throttle(read).await;
        writer.write_all(&buffer[..read]).await?;
        copied = copied.saturating_add(read as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bandwidth::BandwidthRegistry;
    use crate::policy::NetworkBandwidthLimit;

    #[test]
    fn detects_explicit_proxy_requests() {
        let uri: Uri = "http://example.com/path".parse().unwrap();
        assert!(is_forward_proxy_request(&Method::GET, &uri));
        assert!(!is_forward_proxy_request(
            &Method::GET,
            &"/v1/messages".parse().unwrap()
        ));
    }

    #[test]
    fn connect_target_defaults_to_https_port() {
        assert_eq!(
            parse_connect_target("example.com").unwrap(),
            ConnectTarget {
                host: "example.com".into(),
                port: 443,
                authority: "example.com:443".into(),
            }
        );
        assert_eq!(
            parse_connect_target("[::1]").unwrap(),
            ConnectTarget {
                host: "::1".into(),
                port: 443,
                authority: "[::1]:443".into(),
            }
        );
        assert_eq!(parse_connect_target("example.com:8443").unwrap().port, 8443);
        assert!(parse_connect_target(":443").is_err());
        assert!(parse_connect_target("example.com:0").is_err());
        assert!(parse_connect_target("user@example.com:443").is_err());
    }

    #[test]
    fn rejects_malformed_connect_authorities() {
        for authority in [
            "",
            ":443",
            "example.com:0",
            "example.com:65536",
            "user@example.com:443",
            "example.com/path",
            "[::1",
            "::1:443",
            "example.com:443 extra",
        ] {
            assert!(
                parse_connect_target(authority).is_err(),
                "accepted malformed authority {authority:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn limited_copy_delays_and_preserves_tunnel_bytes() {
        let registry = BandwidthRegistry::default();
        let bandwidth = registry
            .session(vec![NetworkBandwidthLimit {
                host: None,
                port: None,
                bytes_per_second: 1_000,
            }])
            .await;
        let (mut source_writer, source_reader) = tokio::io::duplex(2_048);
        let (destination_writer, mut destination_reader) = tokio::io::duplex(2_048);
        source_writer.write_all(&vec![b'x'; 1_000]).await.unwrap();
        source_writer.shutdown().await.unwrap();
        let copy = tokio::spawn(copy_limited(source_reader, destination_writer, bandwidth));
        let read = tokio::spawn(async move {
            let mut bytes = Vec::new();
            destination_reader.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        tokio::task::yield_now().await;
        assert!(!read.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(copy.await.unwrap().unwrap(), 1_000);
        let bytes = read.await.unwrap();
        assert_eq!(bytes, vec![b'x'; 1_000]);
    }
}
