//! Blocking cache client and service discovery.
use super::protocol::{Envelope, hash, read_frame, write_frame};
use super::transport::{
    Endpoint, Stream, TIMEOUT, TOKEN_ENV, default_endpoint, endpoint, endpoint_from_env,
};
use super::{MAX_READ, Request, Response, SERVER_ENV};
use anyhow::{Context, bail, ensure};
use std::io::Read;
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
#[error("cache connection failed: {0}")]
struct CacheConnectError(#[source] std::io::Error);

/// Blocking client; call from the host side, outside filesystem operation locks.
pub struct CacheClient {
    pub(super) endpoint: String,
    token: Option<String>,
}
impl CacheClient {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::new(endpoint_from_env()?, std::env::var(TOKEN_ENV).ok())
    }
    pub fn new(address: String, token: Option<String>) -> anyhow::Result<Self> {
        if matches!(endpoint(&address)?, Endpoint::Tcp(_)) {
            ensure!(
                token.as_ref().is_some_and(|s| !s.is_empty()),
                "TCP requires {TOKEN_ENV}"
            );
        }
        Ok(Self {
            endpoint: address,
            token,
        })
    }
    /// Discover the default socket, or require an explicitly configured service.
    pub(crate) fn discover() -> anyhow::Result<Option<Self>> {
        let explicit = match std::env::var(SERVER_ENV) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(error.into()),
        };
        if explicit.as_deref() == Some("off") {
            return Ok(None);
        }
        let address = explicit.clone().map_or_else(default_endpoint, Ok)?;
        Self::probe(address, std::env::var(TOKEN_ENV).ok(), explicit.is_some())
    }

    pub(super) fn probe(
        address: String,
        token: Option<String>,
        explicit: bool,
    ) -> anyhow::Result<Option<Self>> {
        let client = Self::new(address, token)?;
        match client.request_timeout(Request::Ping, Duration::from_secs(2)) {
            Ok((Response::Ready, _)) => Ok(Some(client)),
            Ok(_) => bail!("cache server returned an incompatible handshake"),
            Err(error) => {
                let absent = error.downcast_ref::<CacheConnectError>().is_some_and(|e| {
                    matches!(
                        e.0.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    )
                });
                if !explicit && absent {
                    Ok(None)
                } else {
                    Err(error.context("probe shared image cache"))
                }
            }
        }
    }

    /// The returned bytes are present only for `Read`, and are SHA-256 checked.
    pub fn request(&self, request: Request) -> anyhow::Result<(Response, Vec<u8>)> {
        self.request_timeout(request, TIMEOUT)
    }

    fn request_timeout(
        &self,
        request: Request,
        timeout: Duration,
    ) -> anyhow::Result<(Response, Vec<u8>)> {
        let expected = match &request {
            Request::Read { length, .. } => Some(*length),
            _ => None,
        };
        let mut stream = match endpoint(&self.endpoint)? {
            Endpoint::Unix(path) => Stream::Unix(
                UnixStream::connect(path)
                    .map_err(CacheConnectError)
                    .with_context(|| {
                        format!(
                            "connect cache {}; start `pvisor cache serve`",
                            self.endpoint
                        )
                    })?,
            ),
            Endpoint::Tcp(address) => Stream::Tcp(
                TcpStream::connect_timeout(&address, Duration::from_secs(10))
                    .map_err(CacheConnectError)?,
            ),
        };
        stream.timeouts(timeout)?;
        write_frame(
            &mut stream,
            &Envelope {
                version: 1,
                token: self.token.clone(),
                request,
            },
        )?;
        let response: Response = read_frame(&mut stream)?;
        let mut body = Vec::new();
        match &response {
            Response::Error { code, message } => {
                let kind = match code.as_str() {
                    "not_found" => std::io::ErrorKind::NotFound,
                    "permission_denied" => std::io::ErrorKind::PermissionDenied,
                    _ => std::io::ErrorKind::Other,
                };
                return Err(std::io::Error::new(kind, format!("cache {code}: {message}")).into());
            }
            Response::Data { length, sha256 } => {
                ensure!(
                    expected.is_some_and(|limit| *length <= limit) && *length <= MAX_READ,
                    "invalid cache data length"
                );
                body.resize(*length as usize, 0);
                stream.read_exact(&mut body)?;
                ensure!(hash(&body) == *sha256, "cache data digest mismatch");
            }
            _ => ensure!(expected.is_none(), "expected cache data response"),
        }
        Ok((response, body))
    }
}
