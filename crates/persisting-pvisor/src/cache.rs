//! Versioned, read-only OCI file service. See docs/shared-image-cache.md.
use anyhow::{Context, bail, ensure};
use clap::{Args, Subcommand};
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::oci::ImageStore;

mod metadata;
pub(crate) mod progress;
pub use progress::ImageTotals;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod lazy;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use lazy::{LazyMount, prepare_image};

pub const SERVER_ENV: &str = "PERSISTING_PVISOR_CACHE_SERVER";
const TOKEN_ENV: &str = "PERSISTING_PVISOR_CACHE_TOKEN";
const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_READ: u32 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Serve cached OCI files (foreground; Unix socket by default).
    Serve {
        /// unix:///absolute/path or tcp://127.0.0.1:PORT. Defaults to CACHE_SERVER.
        #[arg(long)]
        listen: Option<String>,
        /// OCI cache to serve and populate.
        #[arg(long, env = "PERSISTING_PVISOR_IMAGE_STORE")]
        image_store: Option<PathBuf>,
    },
    /// Resolve and prepare an image on the server; print its immutable digest.
    Prepare { image: String },
    /// List one directory page. Paths are relative to the image root.
    List {
        digest: String,
        path: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Show file attributes without following symlinks.
    Stat { digest: String, path: PathBuf },
    /// Stream one regular file to stdout. Does not follow symlinks.
    Read { digest: String, path: PathBuf },
}

/// One request per connection. All paths are Unix bytes, relative to image root.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Ping,
    Prepare {
        image: String,
        architecture: String,
    },
    List {
        digest: String,
        path: Vec<u8>,
        offset: usize,
    },
    Stat {
        digest: String,
        path: Vec<u8>,
    },
    Read {
        digest: String,
        path: Vec<u8>,
        offset: u64,
        length: u32,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    token: Option<String>,
    request: Request,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ready,
    Prepared {
        #[serde(default)]
        metadata_generation: Option<String>,
        #[serde(default)]
        totals: Option<ImageTotals>,
        digest: String,
        architecture: String,
        env: std::collections::BTreeMap<String, String>,
        entrypoint: Vec<String>,
        cmd: Vec<String>,
    },
    Entries {
        names: Vec<Vec<u8>>,
        next_offset: Option<usize>,
    },
    Metadata {
        kind: String,
        size: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        inode: u64,
        nlink: u64,
        mtime: i64,
        mtime_nsec: i64,
        target: Option<Vec<u8>>,
    },
    Data {
        length: u32,
        sha256: String,
    },
    Error {
        code: String,
        message: String,
    },
}

pub fn default_endpoint() -> anyhow::Result<String> {
    let base = dirs::cache_dir().context("cannot find user cache directory")?;
    Ok(format!(
        "unix://{}",
        base.join("persisting/pvisor/cache.sock").display()
    ))
}

fn endpoint_from_env() -> anyhow::Result<String> {
    match std::env::var(SERVER_ENV) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => default_endpoint(),
        Err(error) => Err(error.into()),
    }
}

fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

enum Endpoint {
    Unix(PathBuf),
    Tcp(SocketAddr),
}

fn endpoint(value: &str) -> anyhow::Result<Endpoint> {
    if let Some(path) = value.strip_prefix("unix://") {
        ensure!(
            Path::new(path).is_absolute(),
            "Unix socket path must be absolute"
        );
        return Ok(Endpoint::Unix(path.into()));
    }
    if let Some(address) = value.strip_prefix("tcp://") {
        let address: SocketAddr = address
            .parse()
            .context("TCP endpoint requires an IP address and port")?;
        ensure!(
            address.ip().is_loopback(),
            "cache TCP is loopback-only; use an SSH tunnel for remote access"
        );
        return Ok(Endpoint::Tcp(address));
    }
    bail!("expected unix:///absolute/path or tcp://127.0.0.1:PORT")
}

enum Stream {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(s) => s.read(buf),
            Self::Tcp(s) => s.read(buf),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(s) => s.write(buf),
            Self::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Unix(s) => s.flush(),
            Self::Tcp(s) => s.flush(),
        }
    }
}
impl Stream {
    fn timeouts(&self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Self::Unix(s) => {
                s.set_read_timeout(Some(timeout))?;
                s.set_write_timeout(Some(timeout))
            }
            Self::Tcp(s) => {
                s.set_read_timeout(Some(timeout))?;
                s.set_write_timeout(Some(timeout))
            }
        }
    }
}

fn read_frame<T: DeserializeOwned>(stream: &mut impl Read) -> anyhow::Result<T> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME,
        "invalid cache frame length"
    );
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn write_frame(stream: &mut impl Write, value: &impl Serialize) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= MAX_FRAME,
        "cache response exceeds frame limit"
    );
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("cache connection failed: {0}")]
struct CacheConnectError(#[source] std::io::Error);

/// Blocking client; call from the host side, outside filesystem operation locks.
pub struct CacheClient {
    endpoint: String,
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

    fn probe(
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
fn hash(bytes: &[u8]) -> String {
    format!("sha256:{}", crate::oci::encode_hex(&Sha256::digest(bytes)))
}

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
    let digest = crate::oci::digest_hex(digest)?;
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

fn handle(store: &ImageStore, request: Request) -> anyhow::Result<(Response, Vec<u8>)> {
    let response = match request {
        Request::Ping => Response::Ready,
        Request::Prepare {
            image,
            architecture: requested,
        } => {
            let image = store.prepare_for_architecture(&image, &requested)?;
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
            let end = offset.saturating_add(256).min(names.len());
            Response::Entries {
                names: names[offset..end].to_vec(),
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
) -> anyhow::Result<()> {
    stream.timeouts(TIMEOUT)?;
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
        handle(store, envelope.request)
    })();
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

fn serve(address: String, store: ImageStore, token: Option<String>) -> anyhow::Result<()> {
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
    for _ in 0..16 {
        let receive = receive.clone();
        let store = store.clone();
        let token = token.clone();
        std::thread::spawn(move || {
            loop {
                let request = receive.lock().unwrap().recv();
                let Ok(stream) = request else { break };
                if let Err(error) = serve_connection(stream, &store, token.as_deref()) {
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

pub fn run(args: CacheArgs) -> anyhow::Result<()> {
    if let CacheCommand::Serve {
        listen,
        image_store,
    } = args.command
    {
        return serve(
            listen.map_or_else(endpoint_from_env, Ok)?,
            ImageStore::new(image_store)?,
            std::env::var(TOKEN_ENV).ok(),
        );
    }
    let client = CacheClient::from_env()?;
    let request = match args.command {
        CacheCommand::Prepare { image } => Request::Prepare {
            image,
            architecture: architecture().into(),
        },
        CacheCommand::List {
            digest,
            path,
            offset,
        } => Request::List {
            digest,
            path: path.unwrap_or_default().as_os_str().as_bytes().to_vec(),
            offset,
        },
        CacheCommand::Stat { digest, path } => Request::Stat {
            digest,
            path: path.as_os_str().as_bytes().to_vec(),
        },
        CacheCommand::Read { digest, path } => {
            let mut offset = 0;
            let mut stdout = std::io::stdout().lock();
            loop {
                let (_, body) = client.request(Request::Read {
                    digest: digest.clone(),
                    path: path.as_os_str().as_bytes().to_vec(),
                    offset,
                    length: MAX_READ,
                })?;
                stdout.write_all(&body)?;
                offset += body.len() as u64;
                if body.len() < MAX_READ as usize {
                    break;
                }
            }
            return Ok(());
        }
        CacheCommand::Serve { .. } => unreachable!(),
    };
    let (response, _) = client.request(request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, ImageStore, String) {
        let tmp = tempfile::tempdir().unwrap();
        let store = ImageStore::new(Some(tmp.path().join("store"))).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let root = store.root.join("rootfs-v3/sha256").join("a".repeat(64));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("hello"), b"hello world").unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        symlink("hello", root.join("alias")).unwrap();
        symlink("/etc", root.join("escape")).unwrap();
        (tmp, store, digest)
    }

    #[test]
    fn server_metadata_reuses_directory_index_and_invalidates_rebuilt_root() {
        let (_tmp, store, digest) = fixture();
        let first = metadata::directory(&store, &digest, b"").unwrap();
        let again = metadata::directory(&store, &digest, b"").unwrap();
        assert!(
            Arc::ptr_eq(&first, &again),
            "directory must not be scanned again"
        );
        let Response::Metadata { size, .. } = metadata::stat(&store, &digest, b"hello").unwrap()
        else {
            panic!()
        };
        assert_eq!(size, 11);
        let generation = metadata::generation(&store, &digest).unwrap();
        let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
        fs::rename(&root, root.with_extension("old")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("hello"), b"new").unwrap();
        assert_ne!(generation, metadata::generation(&store, &digest).unwrap());
        assert_eq!(
            &*metadata::directory(&store, &digest, b"").unwrap(),
            &[b"hello".to_vec()]
        );
        let Response::Metadata { size, .. } = metadata::stat(&store, &digest, b"hello").unwrap()
        else {
            panic!()
        };
        assert_eq!(size, 3);
    }

    #[test]
    fn unix_client_roundtrip_and_parallel_reads() {
        let (tmp, store, digest) = fixture();
        let path = tmp.path().join("server.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            std::thread::scope(|scope| {
                for connection in listener.incoming().take(8) {
                    let store = &store;
                    scope.spawn(move || {
                        serve_connection(Stream::Unix(connection.unwrap()), store, None).unwrap()
                    });
                }
            });
        });
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let address = format!("unix://{}", path.display());
                let digest = digest.clone();
                scope.spawn(move || {
                    let client = CacheClient::new(address, None).unwrap();
                    let (_, body) = client
                        .request(Request::Read {
                            digest,
                            path: b"hello".to_vec(),
                            offset: 6,
                            length: 5,
                        })
                        .unwrap();
                    assert_eq!(body, b"world");
                });
            }
        });
        server.join().unwrap();
    }

    #[test]
    fn paths_metadata_pagination_and_eof() {
        let (_tmp, store, digest) = fixture();
        let (response, _) = handle(
            &store,
            Request::List {
                digest: digest.clone(),
                path: vec![],
                offset: 0,
            },
        )
        .unwrap();
        match response {
            Response::Entries { names, next_offset } => {
                assert_eq!(
                    names,
                    [
                        b"alias".to_vec(),
                        b"dir".to_vec(),
                        b"escape".to_vec(),
                        b"hello".to_vec()
                    ]
                );
                assert!(next_offset.is_none());
            }
            _ => panic!("expected directory"),
        }
        let (response, _) = handle(
            &store,
            Request::Stat {
                digest: digest.clone(),
                path: b"alias".to_vec(),
            },
        )
        .unwrap();
        assert!(
            matches!(response, Response::Metadata { target: Some(target), .. } if target == b"hello")
        );
        for path in [
            b"../hello".as_slice(),
            b"/etc/passwd",
            b"escape/passwd",
            b"alias",
            b"hello\0",
        ] {
            assert!(
                handle(
                    &store,
                    Request::Read {
                        digest: digest.clone(),
                        path: path.to_vec(),
                        offset: 0,
                        length: 10
                    }
                )
                .is_err()
            );
        }
        assert!(
            handle(
                &store,
                Request::Read {
                    digest: "sha256:../../etc".into(),
                    path: b"passwd".to_vec(),
                    offset: 0,
                    length: 10
                }
            )
            .is_err()
        );
        assert!(
            handle(
                &store,
                Request::Read {
                    digest: digest.clone(),
                    path: b"hello".to_vec(),
                    offset: 0,
                    length: MAX_READ + 1
                }
            )
            .is_err()
        );
        let (_, body) = handle(
            &store,
            Request::Read {
                digest,
                path: b"hello".to_vec(),
                offset: 100,
                length: 10,
            },
        )
        .unwrap();
        assert!(body.is_empty());
    }

    #[test]
    fn rejects_bad_frames_versions_and_tokens() {
        assert!(read_frame::<Envelope>(&mut &u32::MAX.to_be_bytes()[..]).is_err());
        assert!(endpoint("tcp://0.0.0.0:9000").is_err());
        assert!(CacheClient::new("tcp://127.0.0.1:9000".into(), None).is_err());
        for (version, token) in [(2, Some("secret")), (1, Some("wrong")), (1, None)] {
            let (_tmp, store, digest) = fixture();
            let (mut client, server) = UnixStream::pair().unwrap();
            let worker = std::thread::spawn(move || {
                serve_connection(Stream::Unix(server), &store, Some("secret")).unwrap()
            });
            write_frame(
                &mut client,
                &Envelope {
                    version,
                    token: token.map(str::to_owned),
                    request: Request::Stat {
                        digest,
                        path: b"hello".to_vec(),
                    },
                },
            )
            .unwrap();
            assert!(matches!(
                read_frame::<Response>(&mut client).unwrap(),
                Response::Error { .. }
            ));
            worker.join().unwrap();
        }
    }

    #[test]
    fn client_rejects_corrupt_content() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("server.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let _: Envelope = read_frame(&mut socket).unwrap();
            write_frame(
                &mut socket,
                &Response::Data {
                    length: 3,
                    sha256: hash(b"abc"),
                },
            )
            .unwrap();
            socket.write_all(b"bad").unwrap();
        });
        let client = CacheClient::new(format!("unix://{}", path.display()), None).unwrap();
        let error = client
            .request(Request::Read {
                digest: "unused".into(),
                path: b"file".to_vec(),
                offset: 0,
                length: 3,
            })
            .unwrap_err();
        assert!(error.to_string().contains("digest mismatch"));
        worker.join().unwrap();
    }
}
