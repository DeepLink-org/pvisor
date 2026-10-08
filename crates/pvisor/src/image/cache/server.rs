//! Authenticated transport for the shared immutable image reader.
use super::portable::PortableCache;
use super::protocol::{Envelope, read_frame, write_frame};
use super::storage::Storage;
use super::transport::{Endpoint, Stream, TIMEOUT, TOKEN_ENV, endpoint};
use super::{Request, Response};
use crate::image::oci::ImageStore;
use anyhow::{Context, bail, ensure};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
#[cfg(test)]
mod tests;

fn reader(store: &ImageStore) -> anyhow::Result<PortableCache> {
    Ok(PortableCache::new(
        Storage::filesystem(store.root.join("cache-v1"), true)?,
        Some(store.root.clone()),
        false,
    ))
}

fn ensure_same_user(socket: &UnixStream) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let uid = {
        let mut uid = 0;
        let mut gid = 0;
        if unsafe { libc::getpeereid(socket.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        uid
    };
    #[cfg(target_os = "linux")]
    let uid = {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        credentials.uid
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("cache peer authentication is supported only on Linux and macOS");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ensure!(
        uid == unsafe { libc::geteuid() },
        "cache socket requires the same user"
    );
    Ok(())
}

const CONNECTION_LIMIT: usize = 64;
const REQUEST_QUEUE_LIMIT: usize = 16;
const FILE_WORKERS: usize = 16;
const PREPARE_WORKERS: usize = 2;
type ExchangeResult = anyhow::Result<(Response, Vec<u8>)>;

// Only legacy V1 preparation hands its stream to a worker. Persistent stream
// reads, writes, idle waits and result waits belong to bounded connection handlers.
enum PrepareReply {
    Connection(Stream, Option<ConnectionPermit>),
    Result(mpsc::SyncSender<ExchangeResult>),
}
type PrepareJob = (PrepareReply, Request);

struct ConnectionPermit(Arc<AtomicUsize>);
impl ConnectionPermit {
    // Keep the atomic update spelling supported by Rust versions before 1.99.
    #[allow(deprecated)]
    fn acquire(active: &Arc<AtomicUsize>) -> anyhow::Result<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < CONNECTION_LIMIT).then_some(count + 1)
            })
            .map_err(|_| anyhow::anyhow!("cache connection limit reached"))?;
        Ok(Self(active.clone()))
    }
}
impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ConnectionHandlers {
    active: Arc<AtomicUsize>,
    store: Arc<PortableCache>,
    token: Arc<Option<String>>,
    files: mpsc::SyncSender<PrepareJob>,
    prepare: mpsc::SyncSender<PrepareJob>,
}
impl ConnectionHandlers {
    fn dispatch(&self, stream: Stream) -> anyhow::Result<std::thread::JoinHandle<()>> {
        // Admission does not queue sockets or spawn a thread above the cap.
        // Excess connections close without reading or executing any request.
        let permit = ConnectionPermit::acquire(&self.active)?;
        let store = self.store.clone();
        let token = self.token.clone();
        let files = self.files.clone();
        let prepare = self.prepare.clone();
        Ok(std::thread::Builder::new()
            .name("cache-connection".into())
            .spawn(move || {
                if let Err(error) = serve_connection_queued(
                    stream,
                    &store,
                    token.as_deref(),
                    Some(&prepare),
                    Some(&files),
                    Some(permit),
                ) {
                    eprintln!("cache connection: {error}");
                }
            })?)
    }
}

fn start_request_workers(
    count: usize,
    execute: impl Fn(Request) -> ExchangeResult + Send + Sync + 'static,
) -> (
    mpsc::SyncSender<PrepareJob>,
    Vec<std::thread::JoinHandle<()>>,
) {
    let (send, receive) = mpsc::sync_channel::<PrepareJob>(REQUEST_QUEUE_LIMIT);
    let receive = Arc::new(Mutex::new(receive));
    let execute = Arc::new(execute);
    let workers = (0..count)
        .map(|_| {
            let receive = receive.clone();
            let execute = execute.clone();
            std::thread::spawn(move || {
                loop {
                    let job = receive.lock().unwrap().recv();
                    let Ok((reply, request)) = job else { break };
                    let result = execute(request);
                    match reply {
                        PrepareReply::Connection(stream, _permit) => {
                            if let Err(error) = reply_result(stream, result) {
                                eprintln!("cache preparation: {error}");
                            }
                        }
                        PrepareReply::Result(send) => {
                            let _ = send.send(result);
                        }
                    }
                }
            })
        })
        .collect();
    (send, workers)
}

#[cfg(test)]
fn serve_connection(
    stream: Stream,
    store: &PortableCache,
    token: Option<&str>,
    prepare: Option<&mpsc::SyncSender<PrepareJob>>,
) -> anyhow::Result<()> {
    serve_connection_queued(stream, store, token, prepare, None, None)
}

fn serve_connection_queued(
    mut stream: Stream,
    store: &PortableCache,
    token: Option<&str>,
    prepare: Option<&mpsc::SyncSender<PrepareJob>>,
    files: Option<&mpsc::SyncSender<PrepareJob>>,
    mut permit: Option<ConnectionPermit>,
) -> anyhow::Result<()> {
    stream.nodelay()?;
    stream.timeouts(Duration::from_secs(5))?;
    if let Stream::Unix(socket) = &stream {
        ensure_same_user(socket)?;
    }
    let mut version = None;
    loop {
        // Also bounds idle persistent connections and incomplete request frames.
        stream.timeouts(Duration::from_secs(5))?;
        let envelope: Envelope = match read_frame(&mut stream) {
            Ok(envelope) => envelope,
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::UnexpectedEof
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                    )
                }) =>
            {
                return Ok(());
            }
            Err(error) => return reply_result(stream, Err(error)),
        };
        stream.timeouts(TIMEOUT)?;
        // Check authorization before version refusal so downgrade cannot mask it.
        if token.is_some() && envelope.token.as_deref() != token {
            write_frame(
                &mut stream,
                &Response::Error {
                    code: "permission_denied".into(),
                    message: "cache authentication failed".into(),
                },
            )?;
            return Ok(());
        }
        if !matches!(envelope.version, 1 | 2)
            || version.is_some_and(|version| version != envelope.version)
        {
            write_frame(
                &mut stream,
                &Response::Error {
                    code: "unsupported_version".into(),
                    message: "unsupported cache protocol version".into(),
                },
            )?;
            return Ok(());
        }
        version = Some(envelope.version);
        let request = envelope.request;
        let result = if matches!(request, Request::Prepare { .. })
            && let Some(queue) = prepare
        {
            if envelope.version == 1 {
                // Keep admission counted across the legacy queue handoff too.
                return match queue
                    .try_send((PrepareReply::Connection(stream, permit.take()), request))
                {
                    Ok(()) => Ok(()),
                    Err(
                        mpsc::TrySendError::Full((PrepareReply::Connection(stream, _permit), _))
                        | mpsc::TrySendError::Disconnected((
                            PrepareReply::Connection(stream, _permit),
                            _,
                        )),
                    ) => reply_result(
                        stream,
                        Err(anyhow::anyhow!(
                            "image preparation queue is busy; retry later"
                        )),
                    ),
                    _ => unreachable!(),
                };
            }
            let (send, receive) = mpsc::sync_channel(1);
            match queue.try_send((PrepareReply::Result(send), request)) {
                Ok(()) => receive
                    .recv_timeout(TIMEOUT)
                    .context("wait for image preparation; outcome may be unknown")?,
                Err(_) => Err(anyhow::anyhow!(
                    "image preparation queue is busy; retry later"
                )),
            }
        } else if let Some(queue) = files {
            let (send, receive) = mpsc::sync_channel(1);
            match queue.try_send((PrepareReply::Result(send), request)) {
                Ok(()) => receive
                    .recv_timeout(TIMEOUT)
                    .context("wait for cache request; outcome may be unknown")?,
                // The request has not been admitted or executed on this path.
                Err(_) => Err(anyhow::anyhow!("cache request queue is busy; retry later")),
            }
        } else {
            store.request(request)
        };
        write_result(&mut stream, result)?;
        if envelope.version == 1 {
            return Ok(());
        }
    }
}

fn reply_result(
    mut stream: Stream,
    result: anyhow::Result<(Response, Vec<u8>)>,
) -> anyhow::Result<()> {
    write_result(&mut stream, result)
}

fn write_result(
    stream: &mut Stream,
    result: anyhow::Result<(Response, Vec<u8>)>,
) -> anyhow::Result<()> {
    let (response, body) = result.unwrap_or_else(|error: anyhow::Error| {
        let code = match error.downcast_ref::<std::io::Error>().map(|e| e.kind()) {
            Some(std::io::ErrorKind::NotFound) => "not_found",
            Some(std::io::ErrorKind::PermissionDenied) => "permission_denied",
            _ => "request_failed",
        };
        (
            Response::Error {
                code: code.into(),
                message: format!("{error:#}"),
            },
            Vec::new(),
        )
    });
    write_frame(stream, &response)?;
    stream.write_all(&body)?;
    Ok(())
}

pub(super) fn serve(
    address: String,
    store: ImageStore,
    token: Option<String>,
) -> anyhow::Result<()> {
    let store = Arc::new(reader(&store)?);
    let token = Arc::new(token);
    // Bind before starting workers so address conflicts fail without orphan workers.
    enum Listener {
        Unix(UnixListener, File),
        Tcp(TcpListener),
    }
    let listener = match endpoint(&address)? {
        Endpoint::Unix(path) => {
            let directory = path.parent().context("socket requires parent directory")?;
            fs::create_dir_all(directory)?;
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path.with_extension("sock.lock"))?;
            lock.try_lock_exclusive()
                .context("cache server already running (socket lock held)")?;
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    ensure!(
                        metadata.file_type().is_socket(),
                        "refusing to replace non-socket path"
                    );
                    match UnixStream::connect(&path) {
                        Ok(_) => bail!("cache socket already in use"),
                        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                            fs::remove_file(&path)?
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let listener = UnixListener::bind(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            Listener::Unix(listener, lock)
        }
        Endpoint::Tcp(address) => {
            ensure!(
                token.as_ref().as_ref().is_some_and(|t| !t.is_empty()),
                "TCP requires {TOKEN_ENV}"
            );
            Listener::Tcp(TcpListener::bind(address)?)
        }
    };
    // File workers only execute admitted requests. Idle peers and V2 preparation
    // waits consume a connection permit, never a file-service worker.
    let file_store = store.clone();
    let (files, _file_workers) =
        start_request_workers(FILE_WORKERS, move |request| file_store.request(request));
    let prepare_store = store.clone();
    let (prepare, _prepare_workers) = start_request_workers(PREPARE_WORKERS, move |request| {
        prepare_store.request(request)
    });
    let handlers = ConnectionHandlers {
        active: Arc::new(AtomicUsize::new(0)),
        store,
        token,
        files,
        prepare,
    };
    eprintln!("pvisor-cache listening on {address}");
    match listener {
        Listener::Unix(listener, _lock) => {
            for stream in listener.incoming() {
                let _ = handlers.dispatch(Stream::Unix(stream?));
            }
        }
        Listener::Tcp(listener) => {
            for stream in listener.incoming() {
                let _ = handlers.dispatch(Stream::Tcp(stream?));
            }
        }
    }
    Ok(())
}
