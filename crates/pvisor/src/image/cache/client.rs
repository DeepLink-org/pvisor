//! Blocking cache client and service discovery.
use super::portable::PortableCache;
use super::protocol::{Envelope, hash, read_frame, write_frame};
use super::storage::Storage;
use super::transport::{Endpoint, Stream, TIMEOUT, TOKEN_ENV, endpoint};
use super::{CacheConfig, MAX_READ, Request, Response, SERVER_ENV};
use anyhow::{Context, bail, ensure};
use std::io::{Read, Write};
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
        self.acquire_mode(timeout, false)
    }

    fn acquire_mode(&self, timeout: Duration, fresh: bool) -> anyhow::Result<ConnectionLease<'_>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        loop {
            let before = state.idle.len();
            state.idle.retain(|(_, since)| since.elapsed() < POOL_IDLE);
            state.connections -= before - state.idle.len();
            if let Some((stream, _)) = state.idle.pop() {
                if !fresh {
                    return Ok(ConnectionLease {
                        pool: self,
                        stream: Some(stream),
                        reusable: true,
                    });
                }
                drop(stream);
                state.connections -= 1;
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
    // Replay policy only, not authorization: private bridge confinement remains
    // mandatory. General clients and older runner descriptors have no policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pinned_read_retry: Option<String>,
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
            pinned_read_retry: None,
        }
    }

    pub(super) fn with_pinned_read_retry(mut self, handle: &str) -> anyhow::Result<Self> {
        super::portable::metadata_prefix(handle)?;
        self.pinned_read_retry = Some(handle.into());
        Ok(self)
    }
}
impl CacheClient {
    pub(super) fn is_socket(&self) -> bool {
        matches!(self.transport, CacheTransport::Server { .. })
    }

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
        if let Some(handle) = &binding.pinned_read_retry {
            super::portable::metadata_prefix(handle)?;
        }
        let mut client = Self::configured(
            binding.address,
            binding.token,
            binding.local_store,
            binding.read_only,
        )?;
        client.binding.pinned_read_retry = binding.pinned_read_retry;
        Ok(client)
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
            pinned_read_retry: None,
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

    /// Returned bodies for `Read` and `Metadata` are bounded and SHA-256 checked.
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
            Request::Read { length, .. } | Request::Metadata { length, .. } => Some(*length),
            _ => None,
        };
        let retry = self
            .binding
            .pinned_read_retry
            .as_deref()
            .is_some_and(|handle| pinned_read(handle, &request));
        // The one-retry limit and total budget are per hop. A downstream retry
        // may start another upstream call with its own budget; no end-to-end
        // deadline or attempt count is carried by the bridge protocol.
        let deadline = retry.then(|| Instant::now() + timeout);
        let mut retries = 0;
        let result = loop {
            // The attempt owns its lease; it is dropped before another reservation.
            let attempt = (|| {
                let budget = || match deadline {
                    Some(deadline) => remaining(deadline),
                    None => Ok(timeout),
                };
                let mut lease = if retries == 0 {
                    pool.acquire(budget()?)?
                } else {
                    pool.acquire_mode(budget()?, true)?
                };
                // A failed handshake must also discard a checked-out stream.
                lease.reusable = false;
                let mut version = 2;
                if lease.stream.is_none() {
                    let mut stream =
                        self.connect_budget(endpoint, budget()?, deadline.is_some())?;
                    let legacy = !pool.enabled || pool.state.lock().unwrap().legacy;
                    if legacy
                        || !negotiate(
                            &mut DeadlineIo {
                                stream: &mut stream,
                                deadline,
                            },
                            token,
                        )?
                    {
                        version = 1;
                        if !legacy {
                            pool.state.lock().unwrap().legacy = true;
                            // Never send the actual request on a rejected handshake stream.
                            drop(stream);
                            stream =
                                self.connect_budget(endpoint, budget()?, deadline.is_some())?;
                        }
                    }
                    lease.stream = Some(stream);
                }
                // Only a fully framed, verified exchange may return to the pool.
                let stream = lease.stream.as_mut().unwrap();
                stream.timeouts(budget()?)?;
                let mut stream = DeadlineIo { stream, deadline };
                write_frame(
                    &mut stream,
                    &RequestEnvelope {
                        version,
                        token,
                        request: &request,
                    },
                )?;
                let result = receive_response(&mut stream, expected)?;
                lease.reusable = version == 2;
                Ok::<_, anyhow::Error>(result)
            })();
            match attempt {
                Ok(result) => break result,
                Err(error) if retry && retries == 0 && transport_disconnect(&error) => {
                    retries = 1;
                }
                Err(error) if retry => {
                    return Err(error.context(format!(
                        "pinned cache read failed after {retries} retry (maximum 1)"
                    )));
                }
                Err(error) => return Err(error),
            }
        };
        // Framed application errors are never transport retry candidates.
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

    fn connect_budget(
        &self,
        endpoint: &Endpoint,
        timeout: Duration,
        bounded: bool,
    ) -> anyhow::Result<Stream> {
        let stream = match endpoint {
            Endpoint::Unix(path) => Stream::Unix(
                (if bounded {
                    connect_unix(path, timeout.min(Duration::from_secs(10)))
                } else {
                    UnixStream::connect(path)
                })
                .map_err(CacheConnectError)
                .with_context(|| {
                    format!(
                        "connect cache {}; start `pvisor-cache serve`",
                        self.address()
                    )
                })?,
            ),
            Endpoint::Tcp(address) => Stream::Tcp(
                TcpStream::connect_timeout(
                    address,
                    if bounded {
                        timeout.min(Duration::from_secs(10))
                    } else {
                        Duration::from_secs(10)
                    },
                )
                .map_err(CacheConnectError)?,
            ),
        };
        stream.nodelay()?;
        stream.timeouts(timeout)?;
        Ok(stream)
    }
}

#[derive(serde::Serialize)]
struct RequestEnvelope<'a> {
    version: u32,
    token: &'a Option<String>,
    request: &'a Request,
}

pub(super) fn pinned_read(handle: &str, request: &Request) -> bool {
    match request {
        Request::Stat { digest, .. }
        | Request::List { digest, .. }
        | Request::Read { digest, .. } => digest == handle,
        Request::Metadata {
            handle: requested,
            object_name,
            ..
        } => requested == handle && super::portable::metadata_object_name(object_name),
        _ => false,
    }
}

fn transport_disconnect(error: &anyhow::Error) -> bool {
    let kind = error
        .downcast_ref::<CacheConnectError>()
        .map(|e| e.0.kind())
        .or_else(|| {
            error
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind)
        });
    matches!(
        kind,
        Some(
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected
        )
    ) || (error.downcast_ref::<CacheConnectError>().is_some()
        && kind == Some(std::io::ErrorKind::ConnectionRefused))
}

fn remaining(deadline: Instant) -> std::io::Result<Duration> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "pinned cache read deadline exhausted",
        ))
    } else {
        Ok(duration)
    }
}

// Reset each syscall's timeout to the remaining total budget, including read_exact
// and write_all loops. Generic callers retain their existing per-I/O timeout.
struct DeadlineIo<'a> {
    stream: &'a mut Stream,
    deadline: Option<Instant>,
}
impl DeadlineIo<'_> {
    fn update(&self) -> std::io::Result<()> {
        if let Some(deadline) = self.deadline {
            self.stream.timeouts(remaining(deadline)?)?;
        }
        Ok(())
    }
}
impl Read for DeadlineIo<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.update()?;
        self.stream.read(bytes)
    }
}
impl Write for DeadlineIo<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.update()?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.update()?;
        self.stream.flush()
    }
}

fn connect_unix(path: &std::path::Path, timeout: Duration) -> std::io::Result<UnixStream> {
    use std::os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    };
    let deadline = Instant::now() + timeout;
    // SAFETY: sockaddr is initialized and bounded below; OwnedFd closes every
    // failure path. Nonblocking connect/poll bounds even a full Unix backlog.
    unsafe {
        let mut address: libc::sockaddr_un = std::mem::zeroed();
        let bytes = path.as_os_str().as_bytes();
        if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid Unix cache socket path",
            ));
        }
        address.sun_family = libc::AF_UNIX as _;
        for (destination, byte) in address.sun_path.iter_mut().zip(bytes) {
            *destination = *byte as _;
        }
        let length = std::mem::size_of_val(&address) as libc::socklen_t;
        #[cfg(target_os = "macos")]
        {
            address.sun_len = length as _;
        }
        let raw = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = OwnedFd::from_raw_fd(raw);
        if libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) < 0
            || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length,
        ) < 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error);
            }
            loop {
                let budget = remaining(deadline)?;
                let mut poll = libc::pollfd {
                    fd: fd.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let milliseconds =
                    budget.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
                let result = libc::poll(&mut poll, 1, milliseconds);
                if result < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if result == 0 {
                    remaining(deadline)?;
                    continue;
                }
                let mut error: libc::c_int = 0;
                let mut size = std::mem::size_of_val(&error) as libc::socklen_t;
                if libc::getsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&mut error as *mut libc::c_int).cast(),
                    &mut size,
                ) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if error != 0 {
                    return Err(std::io::Error::from_raw_os_error(error));
                }
                break;
            }
        }
        let stream = UnixStream::from(fd);
        stream.set_nonblocking(false)?;
        Ok(stream)
    }
}

// Only the old service's precise version refusal (or EOF during Ping) permits
// downgrade. Authentication, malformed frames and timeouts remain visible.
fn negotiate(stream: &mut (impl Read + Write), token: &Option<String>) -> anyhow::Result<bool> {
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
    stream: &mut impl Read,
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
            // Reject malformed declarations before a truncated body can turn
            // this framing error into a retryable transport EOF.
            let hex = crate::image::oci::digest_hex(sha256)
                .context("invalid cache data SHA-256 declaration")?;
            ensure!(
                !hex.bytes().any(|byte| byte.is_ascii_uppercase()),
                "invalid cache data SHA-256 declaration: expected lowercase hex"
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
pub(super) mod retry_tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    pub(in crate::image::cache) fn handle() -> String {
        format!(
            "pvisor-v1:{}:linux-amd64:{}",
            "a".repeat(64),
            "b".repeat(64)
        )
    }
    fn read() -> Request {
        Request::Read {
            digest: handle(),
            path: b"file".to_vec(),
            offset: 0,
            length: 8,
        }
    }
    fn pinned(path: &std::path::Path) -> CacheClient {
        let binding = ClientBinding::unix(path)
            .with_pinned_read_retry(&handle())
            .unwrap();
        tests::use_server_reuse(CacheClient::from_binding(binding).unwrap(), true)
    }
    pub(in crate::image::cache) fn accept(listener: &UnixListener) -> UnixStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    return socket;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "expected a fresh cache connection"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("{error}"),
            }
        }
    }
    fn data(socket: &mut UnixStream) {
        write_frame(
            socket,
            &Response::Data {
                length: 8,
                sha256: hash(b"verified"),
            },
        )
        .unwrap();
        socket.write_all(b"verified").unwrap();
    }

    #[test]
    fn binding_compatibility_validation_and_exact_whitelist() {
        let old = serde_json::json!({ "address": "unix:///tmp/cache", "token": null,
            "local_store": null, "read_only": false });
        let binding: ClientBinding = serde_json::from_value(old.clone()).unwrap();
        assert!(binding.pinned_read_retry.is_none());
        assert_eq!(serde_json::to_value(&binding).unwrap(), old);
        assert!(
            binding
                .clone()
                .with_pinned_read_retry("sha256:mutable")
                .is_err()
        );
        let binding = binding.with_pinned_read_retry(&handle()).unwrap();
        let serialized = serde_json::to_value(&binding).unwrap();
        assert_eq!(serialized["pinned_read_retry"], handle());
        let restored: ClientBinding = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored.pinned_read_retry, Some(handle()));
        let mut invalid = old;
        invalid["pinned_read_retry"] = serde_json::json!("bad");
        assert!(CacheClient::from_binding(serde_json::from_value(invalid).unwrap()).is_err());
        for request in [
            read(),
            Request::Stat {
                digest: handle(),
                path: vec![],
            },
            Request::List {
                digest: handle(),
                path: vec![],
                offset: 0,
            },
            Request::Metadata {
                handle: handle(),
                object_name: "index.bin".into(),
                offset: 0,
                length: 8,
            },
        ] {
            assert!(pinned_read(&handle(), &request));
            assert!(!pinned_read(&(handle() + "x"), &request));
        }
        for name in [
            "../index.bin",
            "chunks/secret",
            "INDEX.bin",
            "index.bin/",
            "",
        ] {
            assert!(!pinned_read(
                &handle(),
                &Request::Metadata {
                    handle: handle(),
                    object_name: name.into(),
                    offset: 0,
                    length: 8
                }
            ));
        }
        assert!(!pinned_read(&handle(), &Request::Ping));
    }

    #[test]
    fn cold_and_stale_pooled_disconnects_retry_fresh_identical_verified_reads() {
        for warm in [false, true] {
            for truncated in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let path = temp.path().join("cache.sock");
                let listener = UnixListener::bind(&path).unwrap();
                let client = pinned(&path);
                let server = std::thread::spawn(move || {
                    let mut first = accept(&listener);
                    tests::handshake(&mut first);
                    if warm {
                        assert!(matches!(
                            read_frame::<Envelope>(&mut first).unwrap().request,
                            Request::Ping
                        ));
                        write_frame(&mut first, &Response::Ready).unwrap();
                    }
                    let original: Envelope = read_frame(&mut first).unwrap();
                    if truncated {
                        write_frame(
                            &mut first,
                            &Response::Data {
                                length: 8,
                                sha256: hash(b"verified"),
                            },
                        )
                        .unwrap();
                        first.write_all(b"ver").unwrap();
                    }
                    drop(first);
                    let mut second = accept(&listener);
                    tests::handshake(&mut second);
                    let replay: Envelope = read_frame(&mut second).unwrap();
                    assert_eq!(
                        serde_json::to_value(original).unwrap(),
                        serde_json::to_value(replay).unwrap()
                    );
                    data(&mut second);
                });
                if warm {
                    client.request(Request::Ping).unwrap();
                }
                let (response, body) = client.request(read()).unwrap();
                assert_eq!(body, b"verified");
                assert!(matches!(response, Response::Data { sha256, .. } if sha256 == hash(&body)));
                server.join().unwrap();
            }
        }
    }

    #[test]
    fn malformed_hash_with_truncated_body_never_retries_but_valid_hash_does() {
        let valid = hash(b"verified");
        for sha256 in [
            String::new(),
            "a".repeat(64),
            format!("SHA256:{}", "a".repeat(64)),
            format!("sha512:{}", "a".repeat(64)),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}g", "a".repeat(63)),
            format!("{valid}\n"),
            valid.clone(),
        ] {
            for retry_context in [false, true] {
                let valid_declaration = sha256 == valid;
                let expect_retry = valid_declaration && retry_context;
                let temp = tempfile::tempdir().unwrap();
                let path = temp.path().join("cache.sock");
                let listener = UnixListener::bind(&path).unwrap();
                let check = listener.try_clone().unwrap();
                let client = if retry_context {
                    pinned(&path)
                } else {
                    tests::client_at_with_reuse(&path, true)
                };
                let mut wire = Vec::new();
                write_frame(
                    &mut wire,
                    &Response::Data {
                        length: 8,
                        sha256: sha256.clone(),
                    },
                )
                .unwrap();
                let frame_length = wire.len();
                wire.extend_from_slice(b"ver");
                if !valid_declaration {
                    let mut cursor = std::io::Cursor::new(&wire);
                    let error = receive_response(&mut cursor, Some(8)).unwrap_err();
                    assert!(
                        format!("{error:#}").contains("invalid cache data SHA-256 declaration")
                    );
                    assert_eq!(
                        cursor.position(),
                        frame_length as u64,
                        "malformed hash consumed body bytes"
                    );
                }
                let server = std::thread::spawn(move || {
                    let mut first = accept(&listener);
                    tests::handshake(&mut first);
                    let original: Envelope = read_frame(&mut first).unwrap();
                    first.write_all(&wire).unwrap();
                    drop(first);
                    if expect_retry {
                        let mut fresh = accept(&listener);
                        tests::handshake(&mut fresh);
                        let retry: Envelope = read_frame(&mut fresh).unwrap();
                        assert_eq!(
                            serde_json::to_value(original).unwrap(),
                            serde_json::to_value(retry).unwrap()
                        );
                        data(&mut fresh);
                    }
                });
                let result = client.request_timeout(read(), Duration::from_millis(500));
                if expect_retry {
                    let (response, body) = result.unwrap();
                    assert_eq!(body, b"verified");
                    assert!(
                        matches!(response, Response::Data { sha256, .. } if sha256 == hash(&body))
                    );
                } else {
                    let error = result.unwrap_err();
                    if valid_declaration {
                        assert_eq!(
                            error.downcast_ref::<std::io::Error>().unwrap().kind(),
                            std::io::ErrorKind::UnexpectedEof
                        );
                    } else {
                        assert!(
                            format!("{error:#}").contains("invalid cache data SHA-256 declaration"),
                            "{error:#}"
                        );
                        if retry_context {
                            assert!(error.to_string().contains("after 0 retry"));
                        }
                    }
                }
                server.join().unwrap();
                check.set_nonblocking(true).unwrap();
                assert_eq!(
                    check.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock,
                    "unexpected extra attempt for {sha256:?}"
                );
            }
        }
    }

    #[test]
    fn already_closed_idle_stream_reconnects_instead_of_using_other_pooled_streams() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let client = pinned(&path);
        let (closed, wait) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            tests::handshake(&mut socket);
            assert!(matches!(
                read_frame::<Envelope>(&mut socket).unwrap().request,
                Request::Ping
            ));
            write_frame(&mut socket, &Response::Ready).unwrap();
            drop(socket);
            closed.send(()).unwrap();
            let mut fresh = accept(&listener);
            tests::handshake(&mut fresh);
            assert!(matches!(
                read_frame::<Envelope>(&mut fresh).unwrap().request,
                Request::Read { .. }
            ));
            data(&mut fresh);
        });
        client.request(Request::Ping).unwrap();
        wait.recv().unwrap();
        // Put another live stream below the stale stream in the idle pool. A
        // retry must never select it, even though an ordinary acquisition would.
        let (spare, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let CacheTransport::Server { pool, .. } = &client.transport else {
            panic!()
        };
        {
            let mut state = pool.state.lock().unwrap();
            state.idle.insert(0, (Stream::Unix(spare), Instant::now()));
            state.connections += 1;
        }
        assert_eq!(client.request(read()).unwrap().1, b"verified");
        let mut byte = [0];
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
        server.join().unwrap();
    }

    #[test]
    fn retry_shares_deadline_and_timeout_and_pool_wait_do_not_retry() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let check = listener.try_clone().unwrap();
        let client = pinned(&path);
        let server = std::thread::spawn(move || {
            let mut first = accept(&listener);
            tests::handshake(&mut first);
            let _: Envelope = read_frame(&mut first).unwrap();
            std::thread::sleep(Duration::from_millis(120));
            drop(first);
            let mut fresh = accept(&listener);
            tests::handshake(&mut fresh);
            let _: Envelope = read_frame(&mut fresh).unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        let start = Instant::now();
        let error = client
            .request_timeout(read(), Duration::from_millis(250))
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ));
        assert!(
            start.elapsed() < Duration::from_millis(350),
            "retry multiplied the deadline"
        );
        assert!(error.to_string().contains("after 1 retry"));
        server.join().unwrap();
        check.set_nonblocking(true).unwrap();
        assert_eq!(
            check.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let CacheTransport::Server { pool, .. } = &client.transport else {
            panic!()
        };
        let leases: Vec<_> = (0..POOL_LIMIT)
            .map(|_| pool.acquire(Duration::from_secs(1)).unwrap())
            .collect();
        let error = client
            .request_timeout(read(), Duration::from_millis(20))
            .unwrap_err();
        assert!(error.to_string().contains("after 0 retry"));
        assert!(format!("{error:#}").contains("pool timed out"));
        assert_eq!(
            check.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(leases);
    }

    #[test]
    fn tcp_pinned_read_retries_first_transport_disconnect() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let binding = CacheClient::new(
            format!("tcp://{}", listener.local_addr().unwrap()),
            Some("secret".into()),
        )
        .unwrap()
        .binding()
        .with_pinned_read_retry(&handle())
        .unwrap();
        let client = tests::use_server_reuse(CacheClient::from_binding(binding).unwrap(), true);
        let server = std::thread::spawn(move || {
            let mut original = None;
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                for _ in 0..2 {
                    let envelope: Envelope = read_frame(&mut socket).unwrap();
                    assert!(matches!(envelope.request, Request::Ping));
                    write_frame(&mut socket, &Response::Ready).unwrap();
                }
                let envelope: Envelope = read_frame(&mut socket).unwrap();
                let value = serde_json::to_value(envelope).unwrap();
                if attempt == 0 {
                    original = Some(value);
                } else {
                    assert_eq!(original.as_ref().unwrap(), &value);
                    write_frame(
                        &mut socket,
                        &Response::Data {
                            length: 8,
                            sha256: hash(b"verified"),
                        },
                    )
                    .unwrap();
                    socket.write_all(b"verified").unwrap();
                }
            }
        });
        assert_eq!(client.request(read()).unwrap().1, b"verified");
        server.join().unwrap();
    }

    #[test]
    fn two_disconnects_exhaust_exactly_one_retry_and_release_pool() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let client = pinned(&path);
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let mut socket = accept(&listener);
                tests::handshake(&mut socket);
                assert!(matches!(
                    read_frame::<Envelope>(&mut socket).unwrap().request,
                    Request::Read { .. }
                ));
            }
            let mut socket = accept(&listener);
            tests::handshake(&mut socket);
            assert!(matches!(
                read_frame::<Envelope>(&mut socket).unwrap().request,
                Request::Ping
            ));
            write_frame(&mut socket, &Response::Ready).unwrap();
        });
        let error = client.request(read()).unwrap_err();
        assert!(error.to_string().contains("after 1 retry (maximum 1)"));
        let CacheTransport::Server { pool, .. } = &client.transport else {
            panic!()
        };
        assert_eq!(pool.state.lock().unwrap().connections, 0);
        client.request(Request::Ping).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn framed_errors_corruption_and_effectful_requests_never_retry_with_context() {
        for fault in [
            "not_found",
            "permission_denied",
            "busy",
            "connection_reset",
            "invalid",
            "malformed",
            "oversized",
            "oversized_body",
            "auth",
            "hash",
            "prepare",
            "open",
            "different",
            "unlisted",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("cache.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let check = listener.try_clone().unwrap();
            let client = pinned(&path);
            let server = std::thread::spawn(move || {
                let mut socket = accept(&listener);
                if fault == "auth" {
                    assert!(matches!(
                        read_frame::<Envelope>(&mut socket).unwrap().request,
                        Request::Ping
                    ));
                    write_frame(
                        &mut socket,
                        &Response::Error {
                            code: "permission_denied".into(),
                            message: "handshake authentication failed".into(),
                        },
                    )
                    .unwrap();
                    return;
                }
                tests::handshake(&mut socket);
                let _: Envelope = read_frame(&mut socket).unwrap();
                match fault {
                    "invalid" => socket.write_all(&0u32.to_be_bytes()).unwrap(),
                    "oversized" => socket
                        .write_all(&((super::super::protocol::MAX_FRAME + 1) as u32).to_be_bytes())
                        .unwrap(),
                    "malformed" => {
                        socket.write_all(&1u32.to_be_bytes()).unwrap();
                        socket.write_all(b"{").unwrap();
                    }
                    "oversized_body" => {
                        write_frame(
                            &mut socket,
                            &Response::Data {
                                length: 9,
                                sha256: hash(b"verified!"),
                            },
                        )
                        .unwrap();
                    }
                    "hash" => {
                        write_frame(
                            &mut socket,
                            &Response::Data {
                                length: 8,
                                sha256: hash(b"badbytes"),
                            },
                        )
                        .unwrap();
                        socket.write_all(b"verified").unwrap();
                    }
                    "prepare" | "open" | "different" | "unlisted" => {}
                    code => {
                        write_frame(
                            &mut socket,
                            &Response::Error {
                                code: code.into(),
                                message: "framed failure".into(),
                            },
                        )
                        .unwrap();
                        // A framed error preserves reuse, but cannot trigger replay.
                        assert!(matches!(
                            read_frame::<Envelope>(&mut socket).unwrap().request,
                            Request::Ping
                        ));
                        write_frame(&mut socket, &Response::Ready).unwrap();
                    }
                }
            });
            let request = match fault {
                "prepare" => Request::Prepare {
                    image: "mutable".into(),
                    architecture: "amd64".into(),
                    refresh: false,
                },
                "open" => Request::Open {
                    handle: handle(),
                    architecture: "amd64".into(),
                },
                "different" => Request::Stat {
                    digest: handle() + "x",
                    path: vec![],
                },
                "unlisted" => Request::Metadata {
                    handle: handle(),
                    object_name: "secret".into(),
                    offset: 0,
                    length: 8,
                },
                _ => read(),
            };
            let error = client
                .request_timeout(request, Duration::from_millis(500))
                .unwrap_err();
            if fault == "not_found" {
                assert_eq!(
                    error.downcast_ref::<std::io::Error>().unwrap().kind(),
                    std::io::ErrorKind::NotFound
                );
            }
            if fault == "permission_denied" {
                assert_eq!(
                    error.downcast_ref::<std::io::Error>().unwrap().kind(),
                    std::io::ErrorKind::PermissionDenied
                );
            }
            if ["not_found", "permission_denied", "busy", "connection_reset"].contains(&fault) {
                client.request(Request::Ping).unwrap();
            }
            server.join().unwrap();
            check.set_nonblocking(true).unwrap();
            assert_eq!(
                check.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "unexpected replay: {fault}"
            );
        }
    }

    #[test]
    fn only_typed_disconnects_are_retryable() {
        for kind in [
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::Other,
        ] {
            assert!(!transport_disconnect(
                &std::io::Error::new(kind, "test").into()
            ));
        }
        assert!(!transport_disconnect(
            &std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "not a connect phase")
                .into()
        ));
        assert!(transport_disconnect(
            &CacheConnectError(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "connect"
            ))
            .into()
        ));
    }
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

    pub(in crate::image::cache) fn handshake(socket: &mut UnixStream) {
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
