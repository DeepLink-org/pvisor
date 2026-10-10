//! Preserve host cache connectivity while the VM runner has private networking.
//! Only the already pinned image's stat/list/read and metadata requests can cross this socket.
use super::{
    CacheClient, Request, Response,
    client::ClientBinding,
    protocol::{Envelope, read_frame, write_frame},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, Write},
    os::fd::AsRawFd,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const INTERNAL_ENV: &str = "PVISOR_IMAGE_NETWORK_ACCESS";
const BRIDGE_V2_ENV: &str = "PVISOR_LAZY_BRIDGE_V2";
const METRICS_ENV: &str = "PVISOR_LAZY_BRIDGE_METRICS_DIR";
const CONNECTION_LIMIT: usize = 32;
const WORKERS: usize = 4;
const QUEUE_LIMIT: usize = 16;
const WAIT: Duration = Duration::from_secs(300);

fn bridge_v2(value: Option<&std::ffi::OsStr>) -> bool {
    value != Some(std::ffi::OsStr::new("0"))
}

// Private benchmark ABI: 16 native-endian u64 words, schema=1 followed by
// accepted_connections, v1_frames, v2_frames, forwarded_stat/list/read/metadata,
// queue_wait_ns, upstream_execution_ns, rejected_connections, queue_full,
// active_connections, active_workers, queued_requests, reserved. Read after
// reaping for stable totals; live snapshots are intentionally not transactional.
// The MAP_SHARED file itself is the snapshot: exit/_exit/SIGKILL need no Drop,
// flush thread or per-request file I/O. Never truncate a file while it is mapped.
struct Metrics {
    words: std::ptr::NonNull<[AtomicU64; 16]>,
}
// SAFETY: the mapping has immutable layout and is accessed only via atomics.
unsafe impl Send for Metrics {}
unsafe impl Sync for Metrics {}
impl Metrics {
    fn create(directory: &Path) -> anyhow::Result<Arc<Self>> {
        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "unsafe bridge metrics directory"
        );
        let file = tempfile::Builder::new()
            .prefix("bridge-")
            .suffix(".stats")
            .tempfile_in(directory)?;
        file.as_file().set_len(128)?;
        // SAFETY: a new private file is sized before mapping, and no other
        // writer receives its path until the fully initialized file is kept.
        let pointer = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                128,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_file().as_raw_fd(),
                0,
            )
        };
        ensure!(
            pointer != libc::MAP_FAILED,
            "map bridge metrics: {}",
            io::Error::last_os_error()
        );
        let metrics = Arc::new(Self {
            words: std::ptr::NonNull::new(pointer.cast()).unwrap(),
        });
        unsafe {
            metrics.words.as_ptr().write(std::array::from_fn(|index| {
                AtomicU64::new(u64::from(index == 0))
            }))
        };
        file.keep()?;
        Ok(metrics)
    }
    fn add(&self, index: usize, value: u64) {
        (unsafe { self.words.as_ref() })[index].fetch_add(value, Ordering::Relaxed);
    }
}
impl Drop for Metrics {
    fn drop(&mut self) {
        // SAFETY: the last Arc owns the mapping, and all references are gone.
        unsafe {
            libc::munmap(self.words.as_ptr().cast(), 128);
        }
    }
}
type Telemetry = Option<Arc<Metrics>>;
fn count(metrics: &Telemetry, index: usize, value: u64) {
    if let Some(metrics) = metrics {
        metrics.add(index, value);
    }
}
fn elapsed(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u64::MAX as u128) as u64
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    client: ClientBinding,
    handle: String,
    socket: PathBuf,
}

pub(super) struct NetworkAccess {
    child: Child,
    _directory: tempfile::TempDir,
    _descriptor: tempfile::NamedTempFile,
}
impl Drop for NetworkAccess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl NetworkAccess {
    pub(super) fn start(
        parent: &Path,
        client: ClientBinding,
        handle: &str,
    ) -> anyhow::Result<(Self, ClientBinding)> {
        super::portable::metadata_prefix(handle)?;
        // Keep sun_path short even when the image store has a long pathname.
        // Only the socket lives here; credentials remain in the hidden owner.
        let directory = tempfile::Builder::new()
            .prefix("pvisor-image-net-")
            .tempdir_in("/tmp")?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let socket = directory.path().join("cache.sock");
        let mut descriptor = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(
            descriptor.as_file_mut(),
            &Spec {
                client,
                handle: handle.into(),
                socket: socket.clone(),
            },
        )?;
        descriptor.flush()?;
        let mut command = Command::new(std::env::current_exe()?);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let parent_pid = std::process::id() as libc::pid_t;
            // SAFETY: the child hook uses only async-signal-safe syscalls.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != parent_pid {
                        return Err(io::Error::from_raw_os_error(libc::ESRCH));
                    }
                    Ok(())
                });
            }
        }
        let child = command
            .env(INTERNAL_ENV, descriptor.path())
            .env_remove("PVISOR_KRUN_RUNNER_SPEC")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut owner = Self {
            child,
            _directory: directory,
            _descriptor: descriptor,
        };
        let mut ready = String::new();
        io::BufReader::new(
            owner
                .child
                .stdout
                .take()
                .context("missing cache access readiness pipe")?,
        )
        .read_line(&mut ready)?;
        ensure!(
            ready == "ready\n",
            "host image cache access failed to start"
        );
        Ok((
            owner,
            ClientBinding::unix(&socket).with_pinned_read_retry(handle)?,
        ))
    }
}

#[cfg(test)]
fn forward(client: &CacheClient, handle: &str, request: Request) -> (Response, Vec<u8>) {
    forward_measured(client, handle, request, &None)
}
fn forward_measured(
    client: &CacheClient,
    handle: &str,
    request: Request,
    metrics: &Telemetry,
) -> (Response, Vec<u8>) {
    if matches!(request, Request::Ping) {
        return (Response::Ready, vec![]);
    }
    let pinned = super::client::pinned_read(handle, &request);
    if !pinned {
        return (
            Response::Error {
                code: "permission_denied".into(),
                message: "only reads of the bound immutable image are allowed".into(),
            },
            vec![],
        );
    }
    let operation = match &request {
        Request::Stat { .. } => 4,
        Request::List { .. } => 5,
        Request::Read { .. } => 6,
        Request::Metadata { .. } => 7,
        _ => unreachable!(),
    };
    count(metrics, operation, 1);
    let start = metrics.as_ref().map(|_| Instant::now());
    let result = client.request(request);
    if let Some(start) = start {
        count(metrics, 9, elapsed(start));
    }
    result.unwrap_or_else(|error| {
        let code = match error.downcast_ref::<io::Error>().map(io::Error::kind) {
            Some(io::ErrorKind::NotFound) => "not_found",
            Some(io::ErrorKind::PermissionDenied) => "permission_denied",
            _ => "request_failed",
        };
        (
            Response::Error {
                code: code.into(),
                message: format!("{error:#}"),
            },
            vec![],
        )
    })
}
#[cfg(test)]
fn connection(socket: UnixStream, client: &CacheClient, handle: &str) -> anyhow::Result<()> {
    legacy_connection(socket, client, handle, &None)
}
fn legacy_connection(
    mut socket: UnixStream,
    client: &CacheClient,
    handle: &str,
    metrics: &Telemetry,
) -> anyhow::Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(WAIT))?;
    let envelope: Envelope = read_frame(&mut socket)?;
    ensure!(envelope.version == 1, "unsupported image cache protocol");
    count(metrics, 2, 1);
    let (response, body) = forward_measured(client, handle, envelope.request, metrics);
    write_frame(&mut socket, &response)?;
    socket.write_all(&body)?;
    Ok(())
}

type Reply = (Response, Vec<u8>);
struct Job {
    request: Request,
    reply: mpsc::SyncSender<Reply>,
    queued: Option<Instant>,
}
struct Permit {
    active: Arc<AtomicUsize>,
    metrics: Telemetry,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
        count(&self.metrics, 12, u64::MAX);
    }
}
struct Scheduler {
    active: Arc<AtomicUsize>,
    jobs: mpsc::SyncSender<Job>,
    metrics: Telemetry,
}
impl Scheduler {
    fn start(
        execute: impl Fn(Request) -> Reply + Send + Sync + 'static,
        metrics: Telemetry,
    ) -> Self {
        let (jobs, receive) = mpsc::sync_channel::<Job>(QUEUE_LIMIT);
        let receive = Arc::new(Mutex::new(receive));
        let execute = Arc::new(execute);
        for _ in 0..WORKERS {
            let receive = receive.clone();
            let execute = execute.clone();
            let metrics = metrics.clone();
            std::thread::spawn(move || {
                loop {
                    let Ok(job) = receive.lock().unwrap().recv() else {
                        break;
                    };
                    count(&metrics, 14, u64::MAX);
                    count(&metrics, 13, 1);
                    if let Some(start) = job.queued {
                        count(&metrics, 8, elapsed(start));
                    }
                    let result = execute(job.request);
                    count(&metrics, 13, u64::MAX);
                    // A single response slot: delivery never waits for guest I/O.
                    let _ = job.reply.try_send(result);
                }
            });
        }
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            jobs,
            metrics,
        }
    }
    #[allow(deprecated)]
    fn dispatch(&self, socket: UnixStream) -> anyhow::Result<std::thread::JoinHandle<()>> {
        count(&self.metrics, 1, 1);
        if self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < CONNECTION_LIMIT).then_some(count + 1)
            })
            .is_err()
        {
            count(&self.metrics, 10, 1);
            anyhow::bail!("bridge connection limit reached");
        }
        count(&self.metrics, 12, 1);
        let permit = Permit {
            active: self.active.clone(),
            metrics: self.metrics.clone(),
        };
        let jobs = self.jobs.clone();
        let metrics = self.metrics.clone();
        Ok(std::thread::Builder::new()
            .name("image-bridge-connection".into())
            .spawn(move || {
                let _permit = permit;
                // Transport/framing failure closes, never retries an actual request.
                let _ = persistent_connection(socket, &jobs, &metrics);
            })?)
    }
}
fn busy() -> Reply {
    (
        Response::Error {
            code: "busy".into(),
            message: "bridge request queue is busy; retry later".into(),
        },
        vec![],
    )
}
fn persistent_connection(
    mut socket: UnixStream,
    jobs: &mpsc::SyncSender<Job>,
    metrics: &Telemetry,
) -> anyhow::Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(WAIT))?;
    let mut version = None;
    loop {
        let envelope: Envelope = read_frame(&mut socket)?;
        if !matches!(envelope.version, 1 | 2)
            || version.is_some_and(|version| version != envelope.version)
        {
            write_frame(
                &mut socket,
                &Response::Error {
                    code: "unsupported_version".into(),
                    message: "unsupported image cache protocol".into(),
                },
            )?;
            return Ok(());
        }
        version = Some(envelope.version);
        count(metrics, if envelope.version == 1 { 2 } else { 3 }, 1);
        let (reply, receive) = mpsc::sync_channel(1);
        let job = Job {
            request: envelope.request,
            reply,
            queued: metrics.as_ref().map(|_| Instant::now()),
        };
        // Increment before publishing so a fast worker cannot underflow the gauge.
        count(metrics, 14, 1);
        let (response, body) = match jobs.try_send(job) {
            Ok(()) => receive
                .recv_timeout(WAIT)
                .context("wait for bridge request; outcome may be unknown")?,
            Err(_) => {
                count(metrics, 14, u64::MAX);
                count(metrics, 11, 1);
                busy()
            }
        };
        write_frame(&mut socket, &response)?;
        socket.write_all(&body)?;
        if envelope.version == 1 {
            return Ok(());
        }
    }
}

pub(crate) fn run_internal_if_requested() -> anyhow::Result<bool> {
    let Some(path) = std::env::var_os(INTERNAL_ENV) else {
        return Ok(false);
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
            && metadata.len() < 1024 * 1024,
        "unsafe host image access descriptor"
    );
    let spec: Spec = serde_json::from_reader(file)?;
    // This client is private to the bridge; forwarding checks confinement before
    // invoking it. Never enable replay on the original general-purpose client.
    let client = CacheClient::from_binding(spec.client.with_pinned_read_retry(&spec.handle)?)?;
    let metrics = std::env::var_os(METRICS_ENV)
        .map(|path| Metrics::create(Path::new(&path)))
        .transpose()?;
    let listener = UnixListener::bind(&spec.socket)?;
    fs::set_permissions(&spec.socket, fs::Permissions::from_mode(0o600))?;
    if bridge_v2(std::env::var_os(BRIDGE_V2_ENV).as_deref()) {
        let handle = spec.handle;
        let execution_metrics = metrics.clone();
        let scheduler = Scheduler::start(
            move |request| forward_measured(&client, &handle, request, &execution_metrics),
            metrics,
        );
        println!("ready");
        io::stdout().flush()?;
        for socket in listener.incoming() {
            let _ = scheduler.dispatch(socket?);
        }
        return Ok(true);
    }
    // Same-binary V1 control retains the original four socket workers and
    // sixteen queued sockets, including their framing/write waits.
    let (send, receive) = mpsc::sync_channel::<(UnixStream, Option<Instant>)>(QUEUE_LIMIT);
    let receive = Arc::new(Mutex::new(receive));
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            let receive = receive.clone();
            let client = &client;
            let handle = &spec.handle;
            let metrics = &metrics;
            scope.spawn(move || {
                loop {
                    let Ok((socket, queued)) = receive.lock().unwrap().recv() else {
                        break;
                    };
                    count(metrics, 14, u64::MAX);
                    count(metrics, 13, 1);
                    if let Some(start) = queued {
                        count(metrics, 8, elapsed(start));
                    }
                    if let Err(error) = legacy_connection(socket, client, handle, metrics) {
                        tracing::debug!("host image access: {error:#}");
                    }
                    count(metrics, 13, u64::MAX);
                    count(metrics, 12, u64::MAX);
                }
            });
        }
        println!("ready");
        io::stdout().flush()?;
        for socket in listener.incoming() {
            let socket = socket?;
            count(&metrics, 1, 1);
            count(&metrics, 12, 1);
            count(&metrics, 14, 1);
            let queued = metrics.as_ref().map(|_| Instant::now());
            if send.try_send((socket, queued)).is_err() {
                count(&metrics, 12, u64::MAX);
                count(&metrics, 14, u64::MAX);
                count(&metrics, 10, 1);
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_private_hops_retry_upstream_disconnect_but_not_bad_bytes() {
        use super::super::client::{retry_tests, tests as client_tests};
        let temp = tempfile::tempdir().unwrap();
        let upstream_path = temp.path().join("upstream.sock");
        let bridge_path = temp.path().join("bridge.sock");
        let upstream_listener = UnixListener::bind(&upstream_path).unwrap();
        let check = upstream_listener.try_clone().unwrap();
        let handle = retry_tests::handle();
        let upstream = client_tests::use_server_reuse(
            CacheClient::from_binding(
                ClientBinding::unix(&upstream_path)
                    .with_pinned_read_retry(&handle)
                    .unwrap(),
            )
            .unwrap(),
            true,
        );
        let upstream_server = std::thread::spawn(move || {
            let mut first = retry_tests::accept(&upstream_listener);
            client_tests::handshake(&mut first);
            let original: Envelope = read_frame(&mut first).unwrap();
            assert!(matches!(original.request, Request::Metadata { .. }));
            write_frame(
                &mut first,
                &Response::Data {
                    length: 8,
                    sha256: super::super::protocol::hash(b"verified"),
                },
            )
            .unwrap();
            first.write_all(b"ver").unwrap();
            drop(first);
            let mut second = retry_tests::accept(&upstream_listener);
            client_tests::handshake(&mut second);
            let retry: Envelope = read_frame(&mut second).unwrap();
            assert_eq!(
                serde_json::to_value(original).unwrap(),
                serde_json::to_value(retry).unwrap()
            );
            write_frame(
                &mut second,
                &Response::Data {
                    length: 8,
                    sha256: super::super::protocol::hash(b"verified"),
                },
            )
            .unwrap();
            second.write_all(b"verified").unwrap();
            assert!(matches!(
                read_frame::<Envelope>(&mut second).unwrap().request,
                Request::Read { .. }
            ));
            write_frame(
                &mut second,
                &Response::Data {
                    length: 8,
                    sha256: super::super::protocol::hash(b"badbytes"),
                },
            )
            .unwrap();
            second.write_all(b"verified").unwrap();
        });
        let bridge_listener = UnixListener::bind(&bridge_path).unwrap();
        let bridge_check = bridge_listener.try_clone().unwrap();
        let bridge_handle = handle.clone();
        let bridge_server = std::thread::spawn(move || {
            let scheduler = Scheduler::start(
                move |request| forward(&upstream, &bridge_handle, request),
                None,
            );
            // One connection carries both exchanges; bad upstream bytes become a
            // framed application error, not a downstream disconnect/replay.
            let socket = retry_tests::accept(&bridge_listener);
            scheduler.dispatch(socket).unwrap().join().unwrap();
        });
        let downstream = client_tests::use_server_reuse(
            CacheClient::from_binding(
                ClientBinding::unix(&bridge_path)
                    .with_pinned_read_retry(&handle)
                    .unwrap(),
            )
            .unwrap(),
            true,
        );
        let (_, body) = downstream
            .request(Request::Metadata {
                handle: handle.clone(),
                object_name: "index.bin".into(),
                offset: 0,
                length: 8,
            })
            .unwrap();
        assert_eq!(body, b"verified");
        let error = downstream
            .request(Request::Read {
                digest: handle,
                path: b"file".to_vec(),
                offset: 0,
                length: 8,
            })
            .unwrap_err();
        assert!(error.to_string().contains("digest mismatch"));
        drop(downstream);
        upstream_server.join().unwrap();
        bridge_server.join().unwrap();
        for listener in [check, bridge_check] {
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn access_is_confined_to_pinned_reads_and_preserves_verified_bytes() {
        let (_temp, server, client, digest) = crate::image::cache::backend::tests::fixture();
        let client = super::super::client::tests::use_server_reuse(client, false);
        for request in [
            Request::Prepare {
                image: "anything".into(),
                architecture: "amd64".into(),
                refresh: false,
            },
            Request::Open {
                handle: digest.clone(),
                architecture: "amd64".into(),
            },
            Request::Metadata {
                handle: "another image".into(),
                object_name: "index.bin".into(),
                offset: 0,
                length: 65536,
            },
            Request::Metadata {
                handle: digest.clone(),
                object_name: "../index.bin".into(),
                offset: 0,
                length: 65536,
            },
            Request::Metadata {
                handle: digest.clone(),
                object_name: "chunks/sha256/secret".into(),
                offset: 0,
                length: 65536,
            },
            Request::Read {
                digest: "another image".into(),
                path: b"large".to_vec(),
                offset: 0,
                length: 16,
            },
        ] {
            assert!(
                matches!(forward(&client, &digest, request).0, Response::Error { code, .. } if code == "permission_denied")
            );
        }
        assert_eq!(server.reads.load(std::sync::atomic::Ordering::Relaxed), 0);
        let (response, body) = forward(
            &client,
            &digest,
            Request::Read {
                digest: digest.clone(),
                path: b"large".to_vec(),
                offset: 0,
                length: 16,
            },
        );
        assert!(matches!(response, Response::Data { length: 16, .. }));
        assert_eq!(body, [42; 16]);
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use std::io::Read;
    use std::sync::atomic::Ordering;

    #[test]
    fn pinned_metadata_crosses_the_v1_proxy_as_verified_bytes_only() {
        let (_temp, server, client, handle, pages) =
            crate::image::cache::backend::tests::portable_fixture(true);
        let client = super::super::client::tests::use_server_reuse(client, false);
        for request in [
            Request::Metadata {
                handle: "wrong".into(),
                object_name: "index.bin".into(),
                offset: 0,
                length: 65536,
            },
            Request::Metadata {
                handle: handle.clone(),
                object_name: "../COMMIT.json".into(),
                offset: 0,
                length: 65536,
            },
            Request::Metadata {
                handle: handle.clone(),
                object_name: "format.json".into(),
                offset: 0,
                length: 65536,
            },
        ] {
            assert!(matches!(forward(&client, &handle, request).0,
                Response::Error { code, .. } if code == "permission_denied"));
        }
        assert_eq!(pages.load(Ordering::Relaxed), 0);
        let (mut guest, proxy) = UnixStream::pair().unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| connection(proxy, &client, &handle).unwrap());
            write_frame(
                &mut guest,
                &Envelope {
                    version: 1,
                    token: None,
                    request: Request::Metadata {
                        handle: handle.clone(),
                        object_name: "COMMIT.json".into(),
                        offset: 0,
                        length: 65536,
                    },
                },
            )
            .unwrap();
            let response: Response = read_frame(&mut guest).unwrap();
            let Response::Data { length, sha256 } = response else {
                panic!()
            };
            let mut bytes = vec![0; length as usize];
            guest.read_exact(&mut bytes).unwrap();
            assert_eq!(crate::image::cache::hash(&bytes), sha256);
            assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_ok());
            assert_eq!(
                guest.read(&mut [0]).unwrap(),
                0,
                "v1 closes after one response"
            );
        });
        assert_eq!(pages.load(Ordering::Relaxed), 1);
        assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod v2_tests {
    use super::*;
    use std::io::Read;
    use std::sync::Condvar;

    fn private_metrics(directory: &Path) -> Arc<Metrics> {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        Metrics::create(directory).unwrap()
    }
    fn wait(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(Instant::now() < deadline, "bridge progress deadline");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn value(metrics: &Arc<Metrics>, index: usize) -> u64 {
        (unsafe { metrics.words.as_ref() })[index].load(Ordering::Relaxed)
    }
    fn connect(
        listener: &UnixListener,
        scheduler: &Scheduler,
    ) -> (UnixStream, std::thread::JoinHandle<()>) {
        let guest =
            UnixStream::connect(listener.local_addr().unwrap().as_pathname().unwrap()).unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        guest
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (socket, _) = listener.accept().unwrap();
        (guest, scheduler.dispatch(socket).unwrap())
    }
    fn send(guest: &mut UnixStream, version: u32, request: Request) {
        write_frame(
            guest,
            &Envelope {
                version,
                token: None,
                request,
            },
        )
        .unwrap();
    }
    fn response(guest: &mut UnixStream) -> Reply {
        let response: Response = read_frame(guest).unwrap();
        let mut bytes = Vec::new();
        if let Response::Data { length, sha256 } = &response {
            bytes.resize(*length as usize, 0);
            guest.read_exact(&mut bytes).unwrap();
            assert_eq!(super::super::hash(&bytes), *sha256);
        }
        (response, bytes)
    }

    #[test]
    fn unix_v2_interleaves_pinned_operations_and_errors_without_replay() {
        let (temp, server, client, handle, pages) =
            super::super::backend::tests::portable_fixture(false);
        let client = super::super::client::tests::use_server_reuse(client, false);
        let metrics = private_metrics(temp.path());
        let execution_metrics = Some(metrics.clone());
        let pinned = handle.clone();
        let scheduler = Scheduler::start(
            move |request| forward_measured(&client, &pinned, request, &execution_metrics),
            Some(metrics.clone()),
        );
        let listener = UnixListener::bind(temp.path().join("bridge.sock")).unwrap();
        let (mut first, first_thread) = connect(&listener, &scheduler);
        let (mut second, second_thread) = connect(&listener, &scheduler);
        for index in 0..3 {
            let guest = if index == 1 { &mut second } else { &mut first };
            send(
                guest,
                2,
                Request::Stat {
                    digest: handle.clone(),
                    path: b"large".to_vec(),
                },
            );
            assert!(
                matches!(response(guest).0, Response::Metadata { size, mode: 0o100640, .. }
                if size == super::super::protocol::MAX_READ as u64 + 9)
            );
            send(
                guest,
                2,
                Request::List {
                    digest: handle.clone(),
                    path: vec![],
                    offset: 0,
                },
            );
            assert!(matches!(response(guest).0, Response::Entries { names, .. }
                if names.contains(&b"large".to_vec()) && names.contains(&b"alias".to_vec())));
            send(
                guest,
                2,
                Request::Read {
                    digest: handle.clone(),
                    path: b"large".to_vec(),
                    offset: 7,
                    length: 19,
                },
            );
            assert_eq!(response(guest).1, [42; 19]);
            send(
                guest,
                2,
                Request::Metadata {
                    handle: handle.clone(),
                    object_name: "COMMIT.json".into(),
                    offset: 0,
                    length: 65536,
                },
            );
            assert!(serde_json::from_slice::<serde_json::Value>(&response(guest).1).is_ok());
            for request in [
                Request::Stat {
                    digest: "wrong".into(),
                    path: vec![],
                },
                Request::List {
                    digest: "wrong".into(),
                    path: vec![],
                    offset: 0,
                },
                Request::Read {
                    digest: "wrong".into(),
                    path: vec![],
                    offset: 0,
                    length: 1,
                },
                Request::Metadata {
                    handle: handle.clone(),
                    object_name: "../COMMIT.json".into(),
                    offset: 0,
                    length: 1,
                },
                Request::Prepare {
                    image: "wrong".into(),
                    architecture: "amd64".into(),
                    refresh: false,
                },
            ] {
                send(guest, 2, request);
                assert!(
                    matches!(response(guest).0, Response::Error { code, .. } if code == "permission_denied")
                );
            }
            send(
                guest,
                2,
                Request::Stat {
                    digest: handle.clone(),
                    path: b"absent".to_vec(),
                },
            );
            assert!(
                matches!(response(guest).0, Response::Error { code, .. } if code == "not_found")
            );
            send(guest, 2, Request::Ping);
            assert!(matches!(response(guest).0, Response::Ready));
        }
        drop((first, second));
        first_thread.join().unwrap();
        second_thread.join().unwrap();
        assert_eq!(server.reads.load(Ordering::Relaxed), 3);
        assert_eq!(pages.load(Ordering::Relaxed), 3);
        assert_eq!(value(&metrics, 1), 2);
        assert_eq!(value(&metrics, 3), 33);
        assert_eq!(value(&metrics, 4), 6);
        for index in [5, 6, 7] {
            assert_eq!(value(&metrics, index), 3);
        }
        assert!(value(&metrics, 8) > 0 && value(&metrics, 9) > 0);
        assert_eq!(value(&metrics, 12), 0);
    }

    #[test]
    fn idle_and_partial_unix_frames_do_not_occupy_execution_workers() {
        let temp = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let executed = calls.clone();
        let scheduler = Scheduler::start(
            move |_| {
                executed.fetch_add(1, Ordering::Relaxed);
                (Response::Ready, vec![])
            },
            None,
        );
        let listener = UnixListener::bind(temp.path().join("bridge.sock")).unwrap();
        let mut idle = Vec::new();
        let mut threads = Vec::new();
        for index in 0..8 {
            let (mut guest, thread) = connect(&listener, &scheduler);
            if index >= 4 {
                // Both incomplete headers and incomplete JSON bodies.
                guest
                    .write_all(if index % 2 == 0 {
                        &[0, 0]
                    } else {
                        &[0, 0, 0, 20, b'{']
                    })
                    .unwrap();
            }
            idle.push(guest);
            threads.push(thread);
        }
        let (mut guest, thread) = connect(&listener, &scheduler);
        send(&mut guest, 2, Request::Ping);
        assert!(matches!(response(&mut guest).0, Response::Ready));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        drop((idle, guest));
        threads.push(thread);
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(scheduler.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn blocked_unix_body_writes_do_not_pin_workers_or_replay_lost_responses() {
        let temp = tempfile::tempdir().unwrap();
        let metrics = private_metrics(temp.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let executed = calls.clone();
        let scheduler = Scheduler::start(
            move |request| {
                executed.fetch_add(1, Ordering::Relaxed);
                if matches!(request, Request::Ping) {
                    return (Response::Ready, vec![]);
                }
                let body = vec![42; super::super::protocol::MAX_READ as usize];
                (
                    Response::Data {
                        length: body.len() as u32,
                        sha256: super::super::hash(&body),
                    },
                    body,
                )
            },
            Some(metrics.clone()),
        );
        let listener = UnixListener::bind(temp.path().join("bridge.sock")).unwrap();
        let mut guests = Vec::new();
        let mut threads = Vec::new();
        for _ in 0..WORKERS {
            let guest =
                UnixStream::connect(listener.local_addr().unwrap().as_pathname().unwrap()).unwrap();
            let (socket, _) = listener.accept().unwrap();
            let size: libc::c_int = 4096;
            // Ensure the body cannot fit in the socket without a guest reader.
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_SNDBUF,
                        (&size as *const libc::c_int).cast(),
                        std::mem::size_of_val(&size) as libc::socklen_t,
                    )
                },
                0
            );
            threads.push(scheduler.dispatch(socket).unwrap());
            let mut guest = guest;
            send(
                &mut guest,
                2,
                Request::Read {
                    digest: "fixture".into(),
                    path: vec![],
                    offset: 0,
                    length: super::super::protocol::MAX_READ,
                },
            );
            guests.push(guest);
        }
        wait(|| calls.load(Ordering::Relaxed) == WORKERS && value(&metrics, 13) == 0);
        let (mut guest, thread) = connect(&listener, &scheduler);
        send(&mut guest, 2, Request::Ping);
        assert!(matches!(response(&mut guest).0, Response::Ready));
        drop((guests, guest));
        threads.push(thread);
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), WORKERS + 1);
        assert_eq!(scheduler.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn unix_worker_queue_and_connection_saturation_is_bounded_and_cleans_up() {
        let temp = tempfile::tempdir().unwrap();
        let metrics = private_metrics(temp.path());
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let executed = calls.clone();
        let scheduler = Scheduler::start(
            move |_| {
                executed.fetch_add(1, Ordering::Relaxed);
                let (lock, ready) = &*worker_gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = ready.wait(open).unwrap();
                }
                (Response::Ready, vec![])
            },
            Some(metrics.clone()),
        );
        let listener = UnixListener::bind(temp.path().join("bridge.sock")).unwrap();
        let mut guests = Vec::new();
        let mut threads = Vec::new();
        for index in 0..CONNECTION_LIMIT {
            let (mut guest, thread) = connect(&listener, &scheduler);
            send(&mut guest, 2, Request::Ping);
            guests.push(guest);
            threads.push(thread);
            if index < WORKERS {
                wait(|| value(&metrics, 13) == (index + 1) as u64);
            }
        }
        wait(|| value(&metrics, 11) == (CONNECTION_LIMIT - WORKERS - QUEUE_LIMIT) as u64);
        assert_eq!(value(&metrics, 13), WORKERS as u64);
        assert_eq!(value(&metrics, 14), QUEUE_LIMIT as u64);
        assert_eq!(scheduler.active.load(Ordering::Acquire), CONNECTION_LIMIT);
        let mut excess =
            UnixStream::connect(listener.local_addr().unwrap().as_pathname().unwrap()).unwrap();
        excess
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (socket, _) = listener.accept().unwrap();
        assert!(scheduler.dispatch(socket).is_err());
        assert_eq!(excess.read(&mut [0]).unwrap(), 0);
        assert_eq!(value(&metrics, 10), 1);
        let (lock, ready) = &*gate;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        let mut busy_count = 0;
        for guest in &mut guests {
            match response(guest).0 {
                Response::Ready => {}
                Response::Error { code, .. } if code == "busy" => busy_count += 1,
                other => panic!("{other:?}"),
            }
            // A framed saturation error is reusable, with no queued replay.
            send(guest, 2, Request::Ping);
            assert!(matches!(response(guest).0, Response::Ready));
        }
        assert_eq!(busy_count, CONNECTION_LIMIT - WORKERS - QUEUE_LIMIT);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            WORKERS + QUEUE_LIMIT + CONNECTION_LIMIT
        );
        drop(guests);
        for thread in threads {
            thread.join().unwrap();
        }
        wait(|| value(&metrics, 13) == 0 && value(&metrics, 14) == 0);
        assert_eq!(value(&metrics, 12), 0);
        assert_eq!(scheduler.active.load(Ordering::Acquire), 0);
        let (mut guest, thread) = connect(&listener, &scheduler);
        send(&mut guest, 1, Request::Ping);
        assert!(matches!(response(&mut guest).0, Response::Ready));
        assert_eq!(guest.read(&mut [0]).unwrap(), 0);
        thread.join().unwrap();
    }

    #[test]
    fn malformed_and_changed_version_close_without_execution_or_replay() {
        let temp = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let executed = calls.clone();
        let scheduler = Scheduler::start(
            move |_| {
                executed.fetch_add(1, Ordering::Relaxed);
                (Response::Ready, vec![])
            },
            None,
        );
        let listener = UnixListener::bind(temp.path().join("bridge.sock")).unwrap();
        for wire in [&[0, 0, 0, 0][..], &[0, 0, 0, 1, b'{'][..]] {
            let (mut guest, thread) = connect(&listener, &scheduler);
            guest.write_all(wire).unwrap();
            assert_eq!(guest.read(&mut [0]).unwrap(), 0);
            thread.join().unwrap();
        }
        let (mut guest, thread) = connect(&listener, &scheduler);
        send(&mut guest, 2, Request::Ping);
        response(&mut guest);
        send(&mut guest, 1, Request::Ping);
        assert!(
            matches!(response(&mut guest).0, Response::Error { code, .. } if code == "unsupported_version")
        );
        assert_eq!(guest.read(&mut [0]).unwrap(), 0);
        thread.join().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    // Re-exec entry for the actual private bridge, not a mock scheduler. Ignored
    // in ordinary runs; the parent explicitly launches this one test.
    #[test]
    #[ignore]
    fn internal_bridge_process() {
        assert!(run_internal_if_requested().unwrap());
    }

    #[test]
    fn subprocess_overrides_are_independent_and_killed_bridge_leaves_metrics() {
        struct ChildOwner(Child);
        impl Drop for ChildOwner {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let (temp, server, client, handle, _) =
            super::super::backend::tests::portable_fixture(false);
        for upstream in ["0", "1"] {
            for bridge in ["0", "1"] {
                let directory = tempfile::tempdir_in(temp.path()).unwrap();
                fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
                let socket = directory.path().join("bridge.sock");
                let mut descriptor = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
                serde_json::to_writer(
                    descriptor.as_file_mut(),
                    &Spec {
                        client: client.binding(),
                        handle: handle.clone(),
                        socket: socket.clone(),
                    },
                )
                .unwrap();
                descriptor.flush().unwrap();
                let test = format!(
                    "{}::internal_bridge_process",
                    module_path!().split_once("::").unwrap().1
                );
                let mut child = ChildOwner(
                    Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", &test, "--ignored", "--nocapture"])
                        .env(INTERNAL_ENV, descriptor.path())
                        .env(BRIDGE_V2_ENV, bridge)
                        .env("PVISOR_LAZY_IMAGE_V2", upstream)
                        .env(METRICS_ENV, directory.path())
                        .env_remove("PVISOR_KRUN_RUNNER_SPEC")
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit())
                        .spawn()
                        .unwrap(),
                );
                let mut ready = io::BufReader::new(child.0.stdout.take().unwrap());
                loop {
                    let mut line = String::new();
                    assert!(
                        ready.read_line(&mut line).unwrap() > 0,
                        "bridge exited before ready"
                    );
                    if line == "ready\n" {
                        break;
                    }
                }
                let mut guest = UnixStream::connect(&socket).unwrap();
                guest
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                send(
                    &mut guest,
                    1,
                    Request::Read {
                        digest: handle.clone(),
                        path: b"large".to_vec(),
                        offset: 0,
                        length: 16,
                    },
                );
                assert_eq!(response(&mut guest).1, [42; 16]);
                assert_eq!(
                    guest.read(&mut [0]).unwrap(),
                    0,
                    "V1 must close in either mode"
                );
                let mut guest = UnixStream::connect(&socket).unwrap();
                guest
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                send(&mut guest, 2, Request::Ping);
                if bridge == "0" {
                    assert_eq!(
                        guest.read(&mut [0]).unwrap(),
                        0,
                        "old bridge refuses V2 by closing"
                    );
                } else {
                    assert!(matches!(response(&mut guest).0, Response::Ready));
                    send(&mut guest, 2, Request::Ping);
                    assert!(matches!(response(&mut guest).0, Response::Ready));
                }
                // Deliberately kill with a live stream: no owner/worker Drop or
                // snapshot hook can run. Shared mapped totals must still exist.
                drop(child);
                let path = fs::read_dir(directory.path())
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.extension()
                            .is_some_and(|extension| extension == "stats")
                    })
                    .unwrap();
                let bytes = fs::read(path).unwrap();
                let words: Vec<_> = bytes
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|word| u64::from_ne_bytes(*word))
                    .collect();
                assert_eq!(words[0], 1);
                assert_eq!(words[1], 2);
                assert_eq!(words[2], 1);
                assert_eq!(words[3], if bridge == "0" { 0 } else { 2 });
                assert_eq!(words[6], 1);
                assert!(words[9] > 0);
            }
        }
        assert_eq!(server.reads.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn legacy_override_is_independent_and_metrics_survive_without_drop_snapshot() {
        for (value, enabled) in [
            (None, true),
            (Some("0"), false),
            (Some("1"), true),
            (Some(""), true),
        ] {
            assert_eq!(bridge_v2(value.map(std::ffi::OsStr::new)), enabled);
        }
        let temp = tempfile::tempdir().unwrap();
        let metrics = private_metrics(temp.path());
        metrics.add(1, 7);
        metrics.add(9, 123);
        let path = fs::read_dir(temp.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        drop(metrics);
        let bytes = fs::read(path).unwrap();
        assert_eq!(bytes.len(), 128);
        let words: Vec<_> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|word| u64::from_ne_bytes(*word))
            .collect();
        assert_eq!(words[0], 1);
        assert_eq!(words[1], 7);
        assert_eq!(words[9], 123);
    }
}
