//! Authenticated file service and bounded request/preparation workers.
use super::protocol::{Envelope, MAX_FRAME, hash, read_frame, write_frame};
use super::transport::{Endpoint, Stream, TIMEOUT, TOKEN_ENV, endpoint};
use super::{MAX_READ, Request, Response, progress};
use crate::image::oci::ImageStore;
use anyhow::{Context, bail, ensure};
use fs2::FileExt;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

mod metadata;
#[cfg(test)]
mod tests;

// Every component is opened relative to its parent fd, without following links.
// This remains confined even if a directory is renamed during a request.
fn open_child(parent: &File, name: &OsStr, directory: bool) -> anyhow::Result<File> {
    let name = CString::new(name.as_bytes())?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn parent(store: &ImageStore, digest: &str, path: &[u8]) -> anyhow::Result<(File, Vec<u8>)> {
    let digest = crate::image::oci::digest_hex(digest)?;
    ensure!(!path.contains(&0), "NUL in cache path");
    let path = Path::new(OsStr::from_bytes(path));
    let components: Vec<_> = path.components().collect();
    ensure!(
        components.iter().all(|c| matches!(c, Component::Normal(_))),
        "cache path must be relative without dot or parent components"
    );
    let roots = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(store.root.join("rootfs-v3/sha256"))?;
    let mut directory = open_child(&roots, OsStr::new(digest), true)?;
    for component in components.iter().take(components.len().saturating_sub(1)) {
        directory = open_child(&directory, component.as_os_str(), true)?;
    }
    let name = components
        .last()
        .map_or_else(|| b".".to_vec(), |c| c.as_os_str().as_bytes().to_vec());
    Ok((directory, name))
}

fn directory_names(directory: File) -> anyhow::Result<Vec<Vec<u8>>> {
    let raw = unsafe { libc::fdopendir(directory.as_raw_fd()) };
    if raw.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let _ = directory.into_raw_fd(); // fdopendir owns the descriptor on success.
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let directory = Directory(raw);
    let mut names = Vec::new();
    loop {
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(directory.0) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(error.into());
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    }
    Ok(names)
}

#[allow(clippy::unnecessary_cast)] // libc stat field widths differ by platform.
fn metadata_at(directory: &File, name: &[u8]) -> anyhow::Result<Response> {
    let name = CString::new(name)?;
    let mut m: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut m,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let kind = match m.st_mode & libc::S_IFMT {
        libc::S_IFREG => "file",
        libc::S_IFDIR => "directory",
        libc::S_IFLNK => "symlink",
        _ => "special",
    };
    let target = if kind == "symlink" {
        let mut bytes = vec![0u8; 4096];
        let size = unsafe {
            libc::readlinkat(
                directory.as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        ensure!(
            (size as usize) < bytes.len(),
            "symlink target exceeds protocol limit"
        );
        bytes.truncate(size as usize);
        Some(bytes)
    } else {
        None
    };
    Ok(Response::Metadata {
        kind: kind.into(),
        size: m.st_size as u64,
        mode: m.st_mode as u32,
        uid: m.st_uid,
        gid: m.st_gid,
        inode: m.st_ino as u64,
        nlink: m.st_nlink as u64,
        mtime: m.st_mtime as i64,
        mtime_nsec: m.st_mtime_nsec as i64,
        target,
    })
}

pub(super) fn handle(store: &ImageStore, request: Request) -> anyhow::Result<(Response, Vec<u8>)> {
    let response = match request {
        Request::Ping => Response::Ready,
        Request::Prepare {
            image,
            architecture: requested,
            refresh,
        } => {
            let image = store.prepare_with_refresh(&image, &requested, refresh)?;
            Response::Prepared {
                metadata_generation: Some(metadata::generation(store, &image.digest)?),
                totals: Some(progress::image_totals(store, &image.digest)?),
                digest: image.digest,
                architecture: requested,
                env: image.env,
                entrypoint: image.entrypoint,
                cmd: image.cmd,
            }
        }
        Request::List {
            digest,
            path,
            offset,
        } => {
            let names = metadata::directory(store, &digest, &path)?;
            ensure!(offset <= names.len(), "directory offset out of range");
            let mut page = Vec::new();
            let mut attributes = Vec::new();
            // Leave room for JSON field names and pagination. Symlink targets and
            // non-UTF-8 names can expand substantially in JSON byte arrays.
            let mut frame_bytes = 128;
            for name in names.iter().skip(offset).take(256) {
                let mut child = path.clone();
                if !child.is_empty() {
                    child.push(b'/');
                }
                child.extend_from_slice(name);
                let attr = metadata::stat(store, &digest, &child)?;
                let bytes = serde_json::to_vec(name)?.len() + serde_json::to_vec(&attr)?.len() + 2;
                if frame_bytes + bytes > MAX_FRAME {
                    ensure!(!page.is_empty(), "directory entry exceeds protocol limit");
                    break;
                }
                frame_bytes += bytes;
                page.push(name.clone());
                attributes.push(attr);
            }
            let end = offset + page.len();
            Response::Entries {
                names: page,
                metadata: Some(attributes),
                next_offset: (end < names.len()).then_some(end),
            }
        }
        Request::Stat { digest, path } => metadata::stat(store, &digest, &path)?,
        Request::Read {
            digest,
            path,
            offset,
            length,
        } => {
            ensure!(
                length > 0 && length <= MAX_READ,
                "read length must be 1..={MAX_READ}"
            );
            let (directory, name) = parent(store, &digest, &path)?;
            let mut file = open_child(&directory, OsStr::from_bytes(&name), false)?;
            ensure!(file.metadata()?.is_file(), "only regular files can be read");
            file.seek(SeekFrom::Start(offset))?;
            let mut body = Vec::new();
            file.take(length as u64).read_to_end(&mut body)?;
            return Ok((
                Response::Data {
                    length: body.len() as u32,
                    sha256: hash(&body),
                },
                body,
            ));
        }
    };
    Ok((response, Vec::new()))
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
    store: &ImageStore,
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
    reply_result(stream, handle(store, request))
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
    let store = Arc::new(store);
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
                if let Err(error) = reply_result(stream, handle(&store, request)) {
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
    eprintln!("pvisor cache listening on {address}");
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
