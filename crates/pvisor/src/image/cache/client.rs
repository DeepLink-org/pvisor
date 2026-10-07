//! Blocking cache client and service discovery.
use super::portable::PortableCache;
use super::protocol::{Envelope, hash, read_frame, write_frame};
use super::storage::Storage;
use super::transport::{Endpoint, Stream, TIMEOUT, TOKEN_ENV, endpoint};
use super::{CacheConfig, MAX_READ, Request, Response, SERVER_ENV};
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
    transport: CacheTransport,
    binding: ClientBinding,
}

enum CacheTransport {
    Server {
        endpoint: Endpoint,
        token: Option<String>,
    },
    Objects(PortableCache),
}

/// Private host-side runner handoff. Never include this in a guest view.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClientBinding {
    address: String,
    token: Option<String>,
    local_store: Option<std::path::PathBuf>,
    read_only: bool,
}
impl ClientBinding {
    pub(super) fn needs_host_network(&self) -> bool {
        self.address.starts_with("s3://") || matches!(endpoint(&self.address), Ok(Endpoint::Tcp(_)))
    }
    pub(super) fn unix(socket: &std::path::Path) -> Self {
        Self {
            address: format!("unix://{}", socket.display()),
            token: None,
            local_store: None,
            read_only: false,
        }
    }
}
impl CacheClient {
    pub(super) fn local_objects_directory(&self) -> Option<std::path::PathBuf> {
        if !matches!(self.transport, CacheTransport::Objects(_)) {
            return None;
        }
        dirs::cache_dir().map(|root| {
            root.join("pvisor/cache-v1/objects")
                .join(&hash(self.address().as_bytes())[7..])
        })
    }
    pub(super) fn address(&self) -> &str {
        &self.binding.address
    }
    pub(super) fn binding(&self) -> ClientBinding {
        self.binding.clone()
    }
    pub(super) fn from_binding(binding: ClientBinding) -> anyhow::Result<Self> {
        Self::configured(
            binding.address,
            binding.token,
            binding.local_store,
            binding.read_only,
        )
    }
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_config(CacheConfig::from_env()?)
    }
    pub fn from_config(config: CacheConfig) -> anyhow::Result<Self> {
        let address = config.address()?;
        Self::configured(
            address,
            std::env::var(TOKEN_ENV).ok(),
            config.image_store,
            config.read_only,
        )
    }
    pub fn new(address: String, token: Option<String>) -> anyhow::Result<Self> {
        Self::configured(address, token, None, false)
    }
    fn configured(
        address: String,
        token: Option<String>,
        local_store: Option<std::path::PathBuf>,
        read_only: bool,
    ) -> anyhow::Result<Self> {
        let binding = ClientBinding {
            address: address.clone(),
            token: token.clone(),
            local_store: local_store.clone(),
            read_only,
        };
        let transport = if let Some(path) = address.strip_prefix("file://") {
            CacheTransport::Objects(PortableCache::new(
                Storage::filesystem(path.into(), !read_only)?,
                local_store,
                read_only,
            ))
        } else if address.starts_with("s3://") {
            CacheTransport::Objects(PortableCache::new(
                Storage::s3(&address)?,
                local_store,
                read_only,
            ))
        } else {
            ensure!(
                !read_only,
                "read-only mode requires filesystem or S3 cache backend"
            );
            let endpoint = endpoint(&address)?;
            if matches!(endpoint, Endpoint::Tcp(_)) {
                ensure!(
                    token.as_ref().is_some_and(|s| !s.is_empty()),
                    "TCP requires {TOKEN_ENV}"
                );
            }
            CacheTransport::Server { endpoint, token }
        };
        let local_objects = dirs::cache_dir().map(|root| {
            root.join("pvisor/cache-v1/objects")
                .join(&hash(address.as_bytes())[7..])
        });
        Ok(Self {
            transport: match transport {
                CacheTransport::Objects(cache) => {
                    CacheTransport::Objects(cache.with_local_objects(local_objects))
                }
                server => server,
            },
            binding,
        })
    }
    /// Discover the default socket, or require an explicitly configured service.
    pub(crate) fn discover() -> anyhow::Result<Option<Self>> {
        if std::env::var(SERVER_ENV).as_deref() == Ok("off")
            && std::env::var_os(super::config::BACKEND_ENV).is_none()
            && std::env::var_os(super::config::LOCATION_ENV).is_none()
        {
            return Ok(None);
        }
        let config = CacheConfig::from_env()?;
        let address = config.address()?;
        if address.starts_with("file://") || address.starts_with("s3://") {
            return Ok(Some(Self::from_config(config)?));
        }
        Self::probe(
            address,
            std::env::var(TOKEN_ENV).ok(),
            CacheConfig::explicit(),
        )
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

    /// Publish the local OCI image even when its remote reference already exists.
    /// Only filesystem and S3 backends support publication without a daemon.
    pub fn publish(
        &self,
        image: &str,
        architecture: &str,
        refresh: bool,
    ) -> anyhow::Result<Response> {
        match &self.transport {
            CacheTransport::Objects(cache) => cache.publish_image(image, architecture, refresh),
            CacheTransport::Server { .. } => bail!("publish requires a filesystem or S3 backend"),
        }
    }

    fn request_timeout(
        &self,
        request: Request,
        timeout: Duration,
    ) -> anyhow::Result<(Response, Vec<u8>)> {
        let (endpoint, token) = match &self.transport {
            CacheTransport::Objects(cache) => return cache.request(request),
            CacheTransport::Server { endpoint, token } => (endpoint, token),
        };
        let expected = match &request {
            Request::Read { length, .. } => Some(*length),
            _ => None,
        };
        let mut stream = match endpoint {
            Endpoint::Unix(path) => Stream::Unix(
                UnixStream::connect(path)
                    .map_err(CacheConnectError)
                    .with_context(|| {
                        format!(
                            "connect cache {}; start `pvisor-cache serve`",
                            self.address()
                        )
                    })?,
            ),
            Endpoint::Tcp(address) => Stream::Tcp(
                TcpStream::connect_timeout(address, Duration::from_secs(10))
                    .map_err(CacheConnectError)?,
            ),
        };
        stream.timeouts(timeout)?;
        write_frame(
            &mut stream,
            &Envelope {
                version: 1,
                token: token.clone(),
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
