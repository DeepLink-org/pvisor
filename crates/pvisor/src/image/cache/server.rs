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

fn serve_connection(
    mut stream: Stream,
    store: &PortableCache,
    token: Option<&str>,
    prepare: Option<&mpsc::SyncSender<(Stream, Request)>>,
) -> anyhow::Result<()> {
    stream.timeouts(Duration::from_secs(5))?;
    if let Stream::Unix(socket) = &stream {
        ensure_same_user(socket)?;
    }
    let result = (|| {
        let envelope: Envelope = read_frame(&mut stream)?;
        ensure!(envelope.version == 1, "unsupported cache protocol version");
        ensure!(
            token.is_none() || envelope.token.as_deref() == token,
            "cache authentication failed"
        );
        Ok(envelope.request)
    })();
    stream.timeouts(TIMEOUT)?;
    let request = match result {
        Ok(request) => request,
        Err(error) => return reply_result(stream, Err(error)),
    };
    if matches!(request, Request::Prepare { .. })
        && let Some(queue) = prepare
    {
        return match queue.try_send((stream, request)) {
            Ok(()) => Ok(()),
            Err(
                mpsc::TrySendError::Full((stream, _))
                | mpsc::TrySendError::Disconnected((stream, _)),
            ) => reply_result(
                stream,
                Err(anyhow::anyhow!(
                    "image preparation queue is busy; retry later"
                )),
            ),
        };
    }
    reply_result(stream, store.request(request))
}

fn reply_result(
    mut stream: Stream,
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
    write_frame(&mut stream, &response)?;
    stream.write_all(&body)?;
    Ok(())
}

pub(super) fn serve(
    address: String,
    store: ImageStore,
    token: Option<String>,
) -> anyhow::Result<()> {
    let (send, receive) = mpsc::sync_channel::<Stream>(16);
    let receive = Arc::new(Mutex::new(receive));
    let store = Arc::new(reader(&store)?);
    let token = Arc::new(token);
    let dispatch = |stream| -> anyhow::Result<()> {
        // A full queue closes the connection instead of allocating unbounded workers.
        send.try_send(stream)
            .map_err(|_| anyhow::anyhow!("cache server busy"))
    };
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
    // Slow registry/extraction work must not consume the file-service workers.
    let (prepare_send, prepare_receive) = mpsc::sync_channel::<(Stream, Request)>(16);
    let prepare_receive = Arc::new(Mutex::new(prepare_receive));
    for _ in 0..2 {
        let receive = prepare_receive.clone();
        let store = store.clone();
        std::thread::spawn(move || {
            loop {
                let job = receive.lock().unwrap().recv();
                let Ok((stream, request)) = job else { break };
                if let Err(error) = reply_result(stream, store.request(request)) {
                    eprintln!("cache preparation: {error}");
                }
            }
        });
    }
    for _ in 0..16 {
        let prepare_send = prepare_send.clone();
        let receive = receive.clone();
        let store = store.clone();
        let token = token.clone();
        std::thread::spawn(move || {
            loop {
                let request = receive.lock().unwrap().recv();
                let Ok(stream) = request else { break };
                if let Err(error) =
                    serve_connection(stream, &store, token.as_deref(), Some(&prepare_send))
                {
                    eprintln!("cache connection: {error}");
                }
            }
        });
    }
    eprintln!("pvisor service cache listening on {address}");
    match listener {
        Listener::Unix(listener, _lock) => {
            for stream in listener.incoming() {
                let _ = dispatch(Stream::Unix(stream?));
            }
        }
        Listener::Tcp(listener) => {
            for stream in listener.incoming() {
                let _ = dispatch(Stream::Tcp(stream?));
            }
        }
    }
    Ok(())
}
