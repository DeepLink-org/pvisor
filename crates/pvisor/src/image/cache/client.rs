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
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const POOL_LIMIT: usize = 4;
// Retire idle streams before the server's five-second idle timeout.
const POOL_IDLE: Duration = Duration::from_secs(4);

#[derive(Default)]
struct PoolState {
    idle: Vec<(Stream, Instant)>,
    connections: usize,
    legacy: bool,
}

struct ConnectionPool {
    state: Mutex<PoolState>,
    available: Condvar,
    enabled: bool,
}

// A reservation counts connecting, checked-out and idle streams alike. Dropping
// it on any exchange error discards the stream and wakes a waiting caller.
struct ConnectionLease<'a> {
    pool: &'a ConnectionPool,
    stream: Option<Stream>,
    reusable: bool,
}

impl ConnectionPool {
    fn acquire(&self, timeout: Duration) -> anyhow::Result<ConnectionLease<'_>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        loop {
            let before = state.idle.len();
            state.idle.retain(|(_, since)| since.elapsed() < POOL_IDLE);
            state.connections -= before - state.idle.len();
            if let Some((stream, _)) = state.idle.pop() {
                return Ok(ConnectionLease {
                    pool: self,
                    stream: Some(stream),
                    reusable: true,
                });
            }
            if state.connections < POOL_LIMIT {
                state.connections += 1;
                return Ok(ConnectionLease {
                    pool: self,
                    stream: None,
                    reusable: false,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "cache connection pool timed out");
            state = self.available.wait_timeout(state, remaining).unwrap().0;
        }
    }
}

impl Drop for ConnectionLease<'_> {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock().unwrap();
        if self.reusable
            && let Some(stream) = self.stream.take()
        {
            state.idle.push((stream, Instant::now()));
        } else {
            state.connections -= 1;
        }
        self.pool.available.notify_one();
    }
}

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
        pool: ConnectionPool,
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
            CacheTransport::Server {
                endpoint,
                token,
                pool: ConnectionPool {
                    state: Mutex::new(PoolState::default()),
                    available: Condvar::new(),
                    enabled: std::env::var_os("PVISOR_LAZY_IMAGE_V2").as_deref()
                        != Some(std::ffi::OsStr::new("0")),
                },
            }
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
        let (endpoint, token, pool) = match &self.transport {
            CacheTransport::Objects(cache) => return cache.request(request),
            CacheTransport::Server {
                endpoint,
                token,
                pool,
            } => (endpoint, token, pool),
        };
        let expected = match &request {
            Request::Read { length, .. } => Some(*length),
            _ => None,
        };
        let mut lease = pool.acquire(timeout)?;
        let mut version = 2;
        if lease.stream.is_none() {
            let mut stream = self.connect(endpoint, timeout)?;
            let legacy = !pool.enabled || pool.state.lock().unwrap().legacy;
            if legacy || !negotiate(&mut stream, token)? {
                version = 1;
                if !legacy {
                    pool.state.lock().unwrap().legacy = true;
                    // Never send the actual request on a rejected handshake stream.
                    drop(stream);
                    stream = self.connect(endpoint, timeout)?;
                }
            }
            lease.stream = Some(stream);
        }
        // Only a fully framed, verified exchange may return to the pool.
        lease.reusable = false;
        let stream = lease.stream.as_mut().unwrap();
        stream.timeouts(timeout)?;
        write_frame(
            stream,
            &Envelope {
                version,
                token: token.clone(),
                request,
            },
        )?;
        let result = receive_response(stream, expected)?;
        lease.reusable = version == 2;
        if let Response::Error { code, message } = &result.0 {
            let kind = match code.as_str() {
                "not_found" => std::io::ErrorKind::NotFound,
                "permission_denied" => std::io::ErrorKind::PermissionDenied,
                _ => std::io::ErrorKind::Other,
            };
            return Err(std::io::Error::new(kind, format!("cache {code}: {message}")).into());
        }
        Ok(result)
    }

    fn connect(&self, endpoint: &Endpoint, timeout: Duration) -> anyhow::Result<Stream> {
        let stream = match endpoint {
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
        stream.nodelay()?;
        stream.timeouts(timeout)?;
        Ok(stream)
    }
}

// Only the old service's precise version refusal (or EOF during Ping) permits
// downgrade. Authentication, malformed frames and timeouts remain visible.
fn negotiate(stream: &mut Stream, token: &Option<String>) -> anyhow::Result<bool> {
    write_frame(
        stream,
        &Envelope {
            version: 2,
            token: token.clone(),
            request: Request::Ping,
        },
    )?;
    match read_frame::<Response>(stream) {
        Ok(Response::Ready) => {
            // Some old fixtures/services ignore the envelope version but still
            // close after Ready. Confirm persistence with another harmless Ping
            // before risking an effectful request on that stream.
            let confirmation = (|| {
                write_frame(
                    stream,
                    &Envelope {
                        version: 2,
                        token: token.clone(),
                        request: Request::Ping,
                    },
                )?;
                read_frame::<Response>(stream)
            })();
            match confirmation {
                Ok(Response::Ready) => Ok(true),
                Ok(Response::Error { code, message }) => bail!("cache {code}: {message}"),
                Ok(_) => bail!("cache server returned an incompatible handshake"),
                Err(error)
                    if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::UnexpectedEof
                                | std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::ConnectionReset
                        )
                    }) =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        }
        Ok(Response::Error { code, message })
            if (code == "request_failed" || code == "unsupported_version")
                && message == "unsupported cache protocol version" =>
        {
            Ok(false)
        }
        Ok(Response::Error { code, message }) => bail!("cache {code}: {message}"),
        Ok(_) => bail!("cache server returned an incompatible handshake"),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn receive_response(
    stream: &mut Stream,
    expected: Option<u32>,
) -> anyhow::Result<(Response, Vec<u8>)> {
    let response: Response = read_frame(stream)?;
    let mut body = Vec::new();
    match &response {
        // Application errors are complete frames with no body, even for Read.
        // Return them as verified exchanges; the caller converts them to the
        // existing io::Error only after marking the connection reusable.
        Response::Error { .. } => {}
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

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Barrier};

    fn client_at(path: &std::path::Path) -> CacheClient {
        client_at_with_reuse(path, true)
    }

    pub(in crate::image::cache) fn client_at_with_reuse(
        path: &std::path::Path,
        enabled: bool,
    ) -> CacheClient {
        use_server_reuse(
            CacheClient::new(format!("unix://{}", path.display()), None).unwrap(),
            enabled,
        )
    }

    pub(in crate::image::cache) fn use_server_reuse(
        mut client: CacheClient,
        enabled: bool,
    ) -> CacheClient {
        // Tests are independent of the parent's private rollout setting.
        let CacheTransport::Server { pool, .. } = &mut client.transport else {
            panic!()
        };
        pool.enabled = enabled;
        client
    }

    fn ready(socket: &mut UnixStream) {
        write_frame(socket, &Response::Ready).unwrap();
    }

    fn handshake(socket: &mut UnixStream) {
        let envelope: Envelope = read_frame(socket).unwrap();
        assert_eq!(envelope.version, 2);
        assert!(matches!(envelope.request, Request::Ping));
        ready(socket);
        let confirmation: Envelope = read_frame(socket).unwrap();
        assert_eq!(confirmation.version, 2);
        assert!(matches!(confirmation.request, Request::Ping));
        ready(socket);
    }

    #[test]
    fn legacy_version_refusal_and_handshake_eof_fall_back_only_before_actual_request() {
        for legacy in 0..3 {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("legacy.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let envelope: Envelope = read_frame(&mut socket).unwrap();
                assert_eq!(envelope.version, 2);
                assert!(matches!(envelope.request, Request::Ping));
                if legacy == 2 {
                    ready(&mut socket);
                } else if legacy == 0 {
                    write_frame(
                        &mut socket,
                        &Response::Error {
                            code: "request_failed".into(),
                            message: "unsupported cache protocol version".into(),
                        },
                    )
                    .unwrap();
                }
                drop(socket);
                for _ in 0..2 {
                    let (mut socket, _) = listener.accept().unwrap();
                    let envelope: Envelope = read_frame(&mut socket).unwrap();
                    assert_eq!(envelope.version, 1);
                    assert!(matches!(envelope.request, Request::Prepare { .. }));
                    ready(&mut socket);
                }
            });
            let client = client_at(&path);
            for _ in 0..2 {
                client
                    .request(Request::Prepare {
                        image: "fixture".into(),
                        architecture: "amd64".into(),
                        refresh: false,
                    })
                    .unwrap();
            }
            let CacheTransport::Server { pool, .. } = &client.transport else {
                panic!()
            };
            let state = pool.state.lock().unwrap();
            assert!(state.legacy);
            assert_eq!(state.connections, 0);
            drop(state);
            worker.join().unwrap();
        }
    }

    #[test]
    fn handshake_authentication_and_malformed_frames_never_downgrade() {
        for malformed in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("refusal.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let _: Envelope = read_frame(&mut socket).unwrap();
                if malformed {
                    socket.write_all(&0u32.to_be_bytes()).unwrap();
                } else {
                    // Even a misleading version message with an auth code must not downgrade.
                    write_frame(
                        &mut socket,
                        &Response::Error {
                            code: "permission_denied".into(),
                            message: "unsupported cache protocol version".into(),
                        },
                    )
                    .unwrap();
                }
            });
            let client = client_at(&path);
            let error = client.request(Request::Ping).unwrap_err();
            assert!(error.to_string().contains(if malformed {
                "frame length"
            } else {
                "permission_denied"
            }));
            let CacheTransport::Server { pool, .. } = &client.transport else {
                panic!()
            };
            let state = pool.state.lock().unwrap();
            assert!(!state.legacy);
            assert_eq!(state.connections, 0);
            drop(state);
            worker.join().unwrap();
        }
    }

    #[test]
    fn failed_actual_requests_are_not_replayed_and_streams_are_discarded() {
        // EOF after Prepare, malformed response frame, corrupt body, truncated
        // body, and invalid data length all invalidate the stream without retry.
        for failure in 0..5 {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("broken.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                handshake(&mut socket);
                let envelope: Envelope = read_frame(&mut socket).unwrap();
                assert_eq!(envelope.version, 2);
                if failure == 0 {
                    assert!(matches!(envelope.request, Request::Prepare { .. }));
                } else {
                    assert!(matches!(envelope.request, Request::Read { .. }));
                    if failure == 1 {
                        socket.write_all(&0u32.to_be_bytes()).unwrap();
                    } else {
                        write_frame(
                            &mut socket,
                            &Response::Data {
                                length: if failure == 4 { 4 } else { 3 },
                                sha256: hash(b"abc"),
                            },
                        )
                        .unwrap();
                        socket
                            .write_all(if failure == 3 { b"a" } else { b"bad" })
                            .unwrap();
                    }
                }
                drop(socket);
                let (mut socket, _) = listener.accept().unwrap();
                handshake(&mut socket);
                let envelope: Envelope = read_frame(&mut socket).unwrap();
                // This is a separate caller's Ping, never a replay of the failure.
                assert!(matches!(envelope.request, Request::Ping));
                ready(&mut socket);
            });
            let client = client_at(&path);
            let request = if failure == 0 {
                Request::Prepare {
                    image: "fixture".into(),
                    architecture: "amd64".into(),
                    refresh: true,
                }
            } else {
                Request::Read {
                    digest: "fixture".into(),
                    path: b"file".to_vec(),
                    offset: 0,
                    length: 3,
                }
            };
            assert!(client.request(request).is_err());
            let CacheTransport::Server { pool, .. } = &client.transport else {
                panic!()
            };
            assert_eq!(pool.state.lock().unwrap().connections, 0);
            client.request(Request::Ping).unwrap();
            worker.join().unwrap();
        }
    }

    #[test]
    fn concurrent_callers_use_four_bounded_connections_without_crossed_bodies() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("parallel.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let overlap = Arc::new(Barrier::new(POOL_LIMIT));
        let worker = std::thread::spawn(move || {
            std::thread::scope(|scope| {
                for _ in 0..POOL_LIMIT {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    let overlap = overlap.clone();
                    scope.spawn(move || {
                        handshake(&mut socket);
                        // A global request lock would deadlock at this barrier.
                        overlap.wait();
                        while let Ok(envelope) = read_frame::<Envelope>(&mut socket) {
                            assert_eq!(envelope.version, 2);
                            let Request::Read { path, .. } = envelope.request else {
                                panic!()
                            };
                            write_frame(
                                &mut socket,
                                &Response::Data {
                                    length: 1,
                                    sha256: hash(&path),
                                },
                            )
                            .unwrap();
                            socket.write_all(&path).unwrap();
                        }
                    });
                }
            });
        });
        let client = client_at(&path);
        std::thread::scope(|scope| {
            for byte in 0..24u8 {
                let client = &client;
                scope.spawn(move || {
                    let (_, body) = client
                        .request(Request::Read {
                            digest: "fixture".into(),
                            path: vec![byte],
                            offset: 0,
                            length: 1,
                        })
                        .unwrap();
                    assert_eq!(body, [byte]);
                });
            }
        });
        let CacheTransport::Server { pool, .. } = &client.transport else {
            panic!()
        };
        assert_eq!(pool.state.lock().unwrap().idle.len(), POOL_LIMIT);
        assert_eq!(pool.state.lock().unwrap().connections, POOL_LIMIT);
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn pool_waits_are_bounded_and_expired_idle_connections_are_retired() {
        let pool = ConnectionPool {
            state: Mutex::new(PoolState::default()),
            available: Condvar::new(),
            enabled: true,
        };
        let leases: Vec<_> = (0..POOL_LIMIT)
            .map(|_| pool.acquire(Duration::from_secs(1)).unwrap())
            .collect();
        assert!(pool.acquire(Duration::from_millis(1)).is_err());
        drop(leases);
        let (socket, _peer) = UnixStream::pair().unwrap();
        {
            let mut state = pool.state.lock().unwrap();
            state.connections = 1;
            state
                .idle
                .push((Stream::Unix(socket), Instant::now() - POOL_IDLE));
        }
        let lease = pool.acquire(Duration::from_secs(1)).unwrap();
        assert!(lease.stream.is_none());
        assert_eq!(pool.state.lock().unwrap().connections, 1);
        drop(lease);
        assert_eq!(pool.state.lock().unwrap().connections, 0);
    }

    #[test]
    fn fully_framed_application_errors_preserve_io_errors_and_connection_reuse() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("application-errors.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let cases = [
            ("not_found", std::io::ErrorKind::NotFound),
            ("permission_denied", std::io::ErrorKind::PermissionDenied),
            ("request_failed", std::io::ErrorKind::Other),
        ];
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            handshake(&mut socket);
            for (code, _) in cases {
                let envelope: Envelope = read_frame(&mut socket).unwrap();
                assert!(matches!(envelope.request, Request::Read { .. }));
                write_frame(
                    &mut socket,
                    &Response::Error {
                        code: code.into(),
                        message: "fixture refusal".into(),
                    },
                )
                .unwrap();
                let envelope: Envelope = read_frame(&mut socket).unwrap();
                assert!(matches!(envelope.request, Request::Ping));
                ready(&mut socket);
            }
        });
        let client = client_at(&path);
        for (code, kind) in cases {
            let error = client
                .request(Request::Read {
                    digest: "fixture".into(),
                    path: b"missing".to_vec(),
                    offset: 0,
                    length: 3,
                })
                .unwrap_err();
            let error = error.downcast_ref::<std::io::Error>().unwrap();
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), format!("cache {code}: fixture refusal"));
            let CacheTransport::Server { pool, .. } = &client.transport else {
                panic!()
            };
            assert_eq!(pool.state.lock().unwrap().idle.len(), 1);
            assert_eq!(pool.state.lock().unwrap().connections, 1);
            client.request(Request::Ping).unwrap();
        }
        worker.join().unwrap();
    }

    #[test]
    fn client_tcp_connections_enable_nodelay_before_pooling() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("tcp://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut socket = Stream::Tcp(socket);
            for _ in 0..3 {
                let envelope: Envelope = read_frame(&mut socket).unwrap();
                assert_eq!(envelope.version, 2);
                assert!(matches!(envelope.request, Request::Ping));
                write_frame(&mut socket, &Response::Ready).unwrap();
            }
        });
        let mut client = CacheClient::new(address, Some("secret".into())).unwrap();
        let CacheTransport::Server { pool, .. } = &mut client.transport else {
            panic!()
        };
        pool.enabled = true;
        client.request(Request::Ping).unwrap();
        let CacheTransport::Server { pool, .. } = &client.transport else {
            panic!()
        };
        let state = pool.state.lock().unwrap();
        assert_eq!(state.idle.len(), 1);
        let Stream::Tcp(socket) = &state.idle[0].0 else {
            panic!()
        };
        assert!(socket.nodelay().unwrap(), "client must enable TCP_NODELAY");
        drop(state);
        worker.join().unwrap();
    }

    #[test]
    fn disabled_reuse_sends_only_v1_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("disabled.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let envelope: Envelope = read_frame(&mut socket).unwrap();
                assert_eq!(envelope.version, 1);
                ready(&mut socket);
            }
        });
        let mut client = client_at(&path);
        let CacheTransport::Server { pool, .. } = &mut client.transport else {
            panic!()
        };
        pool.enabled = false;
        client.request(Request::Ping).unwrap();
        client.request(Request::Ping).unwrap();
        worker.join().unwrap();
    }
}
