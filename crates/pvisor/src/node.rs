//! Same-user, same-host resource ownership. A connection pins one immutable owner.
//! Losing this service is not transparent recovery for live FUSE-backed VMs.
mod registry;
use crate::{
    cache::{CacheBackend, CacheClient, CacheConfig, LazyImage},
    environment_snapshot::{Compatibility, SnapshotRamMount, SnapshotStore},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    sync::Semaphore,
};

const MAX_FRAME: usize = 64 * 1024;
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub socket: PathBuf,
    pub state: PathBuf,
    pub cache_backend: String,
    pub cache_location: Option<String>,
    pub snapshot_roots: Vec<PathBuf>,
    pub max_owners: usize,
    pub warm_owners: usize,
    pub max_sessions: usize,
    pub max_preparations: usize,
    pub max_cache_bytes: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            socket: "node.sock".into(),
            state: "node".into(),
            cache_backend: "filesystem".into(),
            cache_location: None,
            snapshot_roots: Vec::new(),
            max_owners: 32,
            warm_owners: 2,
            max_sessions: 128,
            max_preparations: 4,
            max_cache_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    request: Request,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Image {
        handle: String,
        manifest_digest: String,
    },
    Ram {
        store: PathBuf,
        snapshot_id: String,
        compatibility: Compatibility,
        filesystem_pool: Option<PathBuf>,
    },
    Stats,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Pinned {
        path: PathBuf,
        manifest_digest: Option<String>,
    },
    Stats {
        active_pins: usize,
        live_owners: usize,
        max_owners: usize,
        warm_owners: usize,
        cache_bytes: usize,
        cache_limit: usize,
        cache_misses: usize,
    },
    Error {
        message: String,
    },
}

/// The socket must remain open until the native runner is reaped.
pub struct Pin {
    path: PathBuf,
    digest: Option<String>,
    _session: UnixStream,
}
impl Pin {
    pub fn rootfs(&self) -> &Path {
        &self.path
    }
    pub fn manifest_digest(&self) -> &str {
        self.digest.as_deref().unwrap_or("")
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn acquire(socket: &Path, request: Request) -> anyhow::Result<Self> {
        let mut session = connect(socket)?;
        write_frame(
            &mut session,
            &Envelope {
                version: VERSION,
                request,
            },
        )?;
        match read_frame(&mut session)? {
            Response::Pinned {
                path,
                manifest_digest,
            } => Ok(Self {
                path,
                digest: manifest_digest,
                _session: session,
            }),
            Response::Error { message } => anyhow::bail!("node resource preparation: {message}"),
            _ => anyhow::bail!("invalid node pin response"),
        }
    }
    pub fn image(socket: &Path, handle: &str, digest: &str) -> anyhow::Result<Self> {
        Self::acquire(
            socket,
            Request::Image {
                handle: handle.into(),
                manifest_digest: digest.into(),
            },
        )
    }
    pub fn ram(
        socket: &Path,
        store: &Path,
        snapshot_id: &str,
        compatibility: Compatibility,
        filesystem_pool: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        Self::acquire(
            socket,
            Request::Ram {
                store: store.into(),
                snapshot_id: snapshot_id.into(),
                compatibility,
                filesystem_pool,
            },
        )
    }
}
fn connect(socket: &Path) -> anyhow::Result<UnixStream> {
    let stream = UnixStream::connect(socket).context("connect node resource service")?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}
pub fn stats(socket: &Path) -> anyhow::Result<serde_json::Value> {
    let mut stream = connect(socket)?;
    write_frame(
        &mut stream,
        &Envelope {
            version: VERSION,
            request: Request::Stats,
        },
    )?;
    let response = read_frame(&mut stream)?;
    if let Response::Error { message } = response {
        anyhow::bail!("{message}");
    }
    Ok(serde_json::to_value(response)?)
}
fn write_frame(stream: &mut impl Write, value: &impl Serialize) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "node frame too large");
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
fn read_frame(stream: &mut impl Read) -> anyhow::Result<Response> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME,
        "invalid node frame length"
    );
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

enum Resource {
    Image(LazyImage),
    Ram {
        path: PathBuf,
        _mount: SnapshotRamMount,
    },
}
struct SessionOwner(Option<Arc<Resource>>);
impl Drop for SessionOwner {
    fn drop(&mut self) {
        if let Some(owner) = self.0.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || drop(owner));
            } else {
                drop(owner);
            }
        }
    }
}
impl Resource {
    fn reply(&self) -> Response {
        match self {
            Self::Image(image) => Response::Pinned {
                path: image.rootfs().into(),
                manifest_digest: Some(image.manifest_digest().into()),
            },
            Self::Ram { path, .. } => Response::Pinned {
                path: path.clone(),
                manifest_digest: None,
            },
        }
    }
}
struct Owners {
    config: Config,
    cache: Option<CacheConfig>,
    registry: registry::Registry<Resource>,
    pins: AtomicUsize,
    draining: std::sync::atomic::AtomicBool,
}
impl Owners {
    fn prepare(&self, request: Request) -> anyhow::Result<Arc<Resource>> {
        match request {
            Request::Image {
                handle,
                manifest_digest,
            } => {
                let cache = self
                    .cache
                    .as_ref()
                    .context("node environment cache is disabled")?;
                // Revalidate publication even on a warm hit; ownership is not authorization.
                let response = CacheClient::from_config(cache.clone())?
                    .request(crate::cache::Request::Open {
                        handle: handle.clone(),
                        architecture: match std::env::consts::ARCH {
                            "aarch64" => "arm64",
                            _ => "amd64",
                        }
                        .into(),
                    })?
                    .0;
                ensure!(
                    matches!(response, crate::cache::Response::Prepared { ref digest, .. } if digest == &manifest_digest),
                    "node environment manifest mismatch"
                );
                self.registry
                    .acquire(format!("image:{handle}:{manifest_digest}"), || {
                        let image =
                            crate::cache::open_image_handle_for_host(cache.clone(), &handle)?;
                        ensure!(
                            image.manifest_digest() == manifest_digest,
                            "node environment revision changed"
                        );
                        Ok(Resource::Image(image))
                    })
            }
            Request::Ram {
                store,
                snapshot_id,
                compatibility,
                filesystem_pool,
            } => {
                let store = store.canonicalize()?;
                ensure!(
                    self.config
                        .snapshot_roots
                        .iter()
                        .any(|root| store.starts_with(root)),
                    "snapshot store outside authorized node roots"
                );
                let filesystem_pool = filesystem_pool.map(|p| p.canonicalize()).transpose()?;
                if let Some(pool) = &filesystem_pool {
                    ensure!(
                        self.config
                            .snapshot_roots
                            .iter()
                            .any(|root| pool.starts_with(root)),
                        "filesystem pool outside authorized node roots"
                    );
                }
                let source = match &filesystem_pool {
                    Some(pool) => SnapshotStore::with_filesystem_pool(&store, pool)?,
                    None => SnapshotStore::new(&store)?,
                };
                let published = source.open_for_restore(&snapshot_id, &compatibility)?;
                // Snapshot IDs seal manifest/RAM/machine identities. Revalidate
                // each authorized source, then reuse identical imported copies
                // across Worker-local stores through one backing inode.
                let key = serde_json::to_string(&("ram", &snapshot_id, &compatibility))?;
                self.registry.acquire(key, || {
                    let mut protected = vec![self.config.state.as_path(), store.as_path()];
                    protected.extend(self.config.snapshot_roots.iter().map(PathBuf::as_path));
                    let (mut mount, file) =
                        SnapshotRamMount::runtime(published.ram_reader()?, &protected)?;
                    mount.watch_native_owner_exit(&std::env::current_exe()?)?;
                    let path = mount.ram_path();
                    drop(file);
                    Ok(Resource::Ram {
                        path,
                        _mount: mount,
                    })
                })
            }
            Request::Stats => anyhow::bail!("stats cannot acquire an owner"),
        }
    }
}

/// Refuse to remove existing endpoints; callers explicitly clean stale sockets.
pub(crate) struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl SocketGuard {
    pub(crate) fn bind(path: &Path) -> anyhow::Result<(UnixListener, Self)> {
        let parent = path.parent().context("socket requires parent")?;
        private_directory(parent)?;
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let metadata = fs::symlink_metadata(path)?;
        Ok((
            listener,
            Self {
                path: path.into(),
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        ))
    }
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| (m.dev(), m.ino()) == (self.device, self.inode))
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
pub(crate) fn private_directory(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "node state/socket directory must be private and owned by this user"
    );
    Ok(())
}
pub async fn serve(mut config: Config) -> anyhow::Result<()> {
    ensure!(
        (1..=4096).contains(&config.max_owners)
            && config.max_sessions > 0
            && config.max_sessions <= 4096
            && (1..=64).contains(&config.max_preparations),
        "invalid node service bounds"
    );
    private_directory(&config.state)?;
    crate::cache_budget::configure(config.max_cache_bytes)?;
    config.snapshot_roots = config
        .snapshot_roots
        .iter()
        .map(|root| root.canonicalize())
        .collect::<Result<_, _>>()?;
    let cache = config
        .cache_location
        .as_ref()
        .map(|location| -> anyhow::Result<_> {
            let backend = match config.cache_backend.as_str() {
                "filesystem" => CacheBackend::Filesystem,
                "s3" => CacheBackend::S3,
                _ => anyhow::bail!("node immutable cache requires filesystem or S3 backend"),
            };
            Ok(CacheConfig {
                backend,
                location: location.clone(),
                read_only: true,
                image_store: Some(config.state.join("images")),
            })
        })
        .transpose()?;
    let registry = registry::Registry::new(config.max_owners, config.warm_owners)?;
    let owners = Arc::new(Owners {
        config: config.clone(),
        cache,
        registry,
        pins: AtomicUsize::new(0),
        draining: std::sync::atomic::AtomicBool::new(false),
    });
    let sessions = Arc::new(Semaphore::new(config.max_sessions));
    let preparing = Arc::new(Semaphore::new(config.max_preparations));
    let handshakes = Arc::new(Semaphore::new(16));
    let (listener, _socket) = SocketGuard::bind(&config.socket)?;
    let mut tasks = tokio::task::JoinSet::new();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    let mut observed_denials = 0;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } { continue; }
                let Ok(handshake) = handshakes.clone().try_acquire_owned() else { continue; };
                let (owners, sessions, preparing) = (owners.clone(), sessions.clone(), preparing.clone());
                tasks.spawn(async move { let result = session(stream, owners, sessions, preparing, handshake).await; if let Err(error) = result { tracing::debug!(%error, "node session ended"); } });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            _ = tokio::signal::ctrl_c() => { owners.draining.store(true, Ordering::SeqCst); },
            _ = terminate.recv() => { owners.draining.store(true, Ordering::SeqCst); },
            _ = interval.tick() => {
                if owners.draining.load(Ordering::SeqCst) && owners.pins.load(Ordering::SeqCst) == 0 && preparing.available_permits() == config.max_preparations { break; }
                let (used, limit, misses) = crate::cache_budget::stats();
                if misses > observed_denials && used >= limit / 2 {
                    let retiring = owners.clone();
                    if let Ok(permit) = preparing.clone().try_acquire_owned() {
                        observed_denials = misses;
                        tokio::task::spawn_blocking(move || { let _permit = permit; drop(retiring.registry.trim_idle()); });
                    }
                } else {
                    observed_denials = misses;
                }
            },
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    tokio::task::spawn_blocking(move || drop(owners)).await?;
    Ok(())
}
async fn session(
    mut stream: tokio::net::UnixStream,
    owners: Arc<Owners>,
    sessions: Arc<Semaphore>,
    preparing: Arc<Semaphore>,
    handshake: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    let envelope = tokio::time::timeout(Duration::from_secs(5), async {
        let length = stream.read_u32().await? as usize;
        ensure!(
            length > 0 && length <= MAX_FRAME,
            "invalid node request length"
        );
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).await?;
        Ok::<Envelope, anyhow::Error>(serde_json::from_slice(&bytes)?)
    })
    .await??;
    drop(handshake);
    ensure!(
        envelope.version == VERSION,
        "unsupported node protocol version"
    );
    if matches!(envelope.request, Request::Stats) {
        let (cache_bytes, cache_limit, cache_misses) = crate::cache_budget::stats();
        return reply(
            &mut stream,
            Response::Stats {
                active_pins: owners.pins.load(Ordering::SeqCst),
                live_owners: owners.registry.live(),
                max_owners: owners.config.max_owners,
                warm_owners: owners.config.warm_owners,
                cache_bytes,
                cache_limit,
                cache_misses,
            },
        )
        .await;
    }
    if owners.draining.load(Ordering::SeqCst) {
        return reply(
            &mut stream,
            Response::Error {
                message: "node service is draining".into(),
            },
        )
        .await;
    }
    let Ok(_session) = sessions.try_acquire_owned() else {
        return reply(
            &mut stream,
            Response::Error {
                message: "node session budget exhausted".into(),
            },
        )
        .await;
    };
    let Ok(permit) = preparing.try_acquire_owned() else {
        return reply(
            &mut stream,
            Response::Error {
                message: "node preparation budget exhausted; retry later".into(),
            },
        )
        .await;
    };
    let loading = owners.clone();
    // Keep the preparation permit inside the blocking job: cancellation cannot
    // release capacity while mounting still runs.
    let resource = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // Wrap inside the blocking job so cancellation of its awaiting session
        // cannot drop the last FUSE owner on an async executor thread.
        loading
            .prepare(envelope.request)
            .map(|owner| SessionOwner(Some(owner)))
    })
    .await?;
    let mut resource = match resource {
        Ok(value) => value,
        Err(error) => {
            return reply(
                &mut stream,
                Response::Error {
                    message: format!("{error:#}"),
                },
            )
            .await;
        }
    };
    if owners.draining.load(Ordering::SeqCst) {
        let retiring = resource.0.take();
        tokio::task::spawn_blocking(move || drop(retiring)).await?;
        return reply(
            &mut stream,
            Response::Error {
                message: "node service is draining".into(),
            },
        )
        .await;
    }
    owners.pins.fetch_add(1, Ordering::SeqCst);
    struct Count(Arc<Owners>);
    impl Drop for Count {
        fn drop(&mut self) {
            self.0.pins.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _count = Count(owners);
    let result = async {
        reply(&mut stream, resource.0.as_ref().unwrap().reply()).await?;
        let mut byte = [0];
        ensure!(
            stream.read(&mut byte).await? == 0,
            "pin sessions do not accept additional requests"
        );
        Ok(())
    }
    .await;
    let released = resource.0.take();
    tokio::task::spawn_blocking(move || drop(released)).await?;
    result
}
async fn reply(stream: &mut tokio::net::UnixStream, response: Response) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(&response)?;
    ensure!(bytes.len() <= MAX_FRAME, "node response too large");
    tokio::time::timeout(Duration::from_secs(5), async {
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(&bytes).await
    })
    .await??;
    Ok(())
}
