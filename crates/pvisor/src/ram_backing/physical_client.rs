//! Physical-page ownership independent of the optional publication connection.
//! A retired session's owner still pins its slots until peer exit. If that owner
//! dies, retained read-only file descriptors keep the old backing alive. Never
//! send a retired session's release tokens to a replacement connection.
use super::*;
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

const MAX_LIVE_SESSIONS: usize = 4;
const INITIAL_RETRY: Duration = Duration::from_millis(250);
const MAX_RETRY: Duration = Duration::from_secs(30);

struct PhysicalSession {
    owner: Arc<()>,
    session: Arc<[u8; 16]>,
    file: Arc<File>,
    length: u64,
    pins: Mutex<BTreeMap<(ImageId, u64), usize>>,
}

/// One locally owned page and its exact backing generation. Mapping and restore
/// remain usable after publisher loss or reconnection. Not cloneable: release
/// consumes one reference only after its mapping dependency has been removed.
pub struct PhysicalReference {
    backing: Arc<PhysicalSession>,
    token: u64,
    id: ImageId,
    offset: u64,
}

impl Drop for PhysicalReference {
    fn drop(&mut self) {
        let mut pins = self.backing.pins.lock().unwrap_or_else(|e| e.into_inner());
        let key = (self.id, self.offset);
        if let Some(count) = pins.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                pins.remove(&key);
            }
        }
    }
}

/// Physical store with local read/release and bounded, authenticated reconnect.
///
/// Startup still requires a negotiated physical pool. Transport loss stops new
/// publication, not existing RAM access. A fresh connection gets a fresh session;
/// live references retain their original file and are never rebound by offset.
/// A maximum of four locally live sessions bounds repeated recovery attempts.
///
/// This is a compatibility bridge for the existing mutable-slot protocol:
/// disconnected remote references may remain charged until VM exit. Recoverable
/// v4 services charge process-witnessed retired files conservatively; this client
/// still does not provide sealed segments or re-adoption of old pool contents.
pub struct PhysicalClient {
    client: PoolClient,
    owner: Arc<()>,
    current: Arc<PhysicalSession>,
    sessions: Vec<Weak<PhysicalSession>>,
    endpoint: Option<PathBuf>,
    // Reserve one token slot before acquiring each reference. Queuing a later
    // release must neither allocate nor turn a COW burst into a disconnect.
    pending: VecDeque<u64>,
    outstanding: usize,
    retry_at: Instant,
    retry_delay: Duration,
    degraded_reported: bool,
}

impl PhysicalClient {
    /// Consume an already authenticated, negotiated physical connection.
    /// `endpoint`, when supplied, must have a private same-user parent. Recovery
    /// revalidates that parent, authenticates the peer and renegotiates physical
    /// discovery before replacing the connection. No live VM bytes are read.
    pub fn new(client: PoolClient, endpoint: Option<PathBuf>) -> io::Result<Self> {
        let owner = Arc::new(());
        let current = Self::session(&client, owner.clone())?;
        let endpoint = endpoint
            .map(|path| {
                let name = path
                    .file_name()
                    .ok_or_else(|| invalid("physical recovery endpoint has no filename"))?;
                let parent = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."))
                    .canonicalize()?;
                let path = parent.join(name);
                validate_endpoint(&path)?;
                Ok::<_, io::Error>(path)
            })
            .transpose()?;
        Ok(Self {
            client,
            owner,
            sessions: vec![Arc::downgrade(&current)],
            current,
            endpoint,
            pending: VecDeque::new(),
            outstanding: 0,
            retry_at: Instant::now(),
            retry_delay: INITIAL_RETRY,
            degraded_reported: false,
        })
    }

    fn session(client: &PoolClient, owner: Arc<()>) -> io::Result<Arc<PhysicalSession>> {
        Ok(Arc::new(PhysicalSession {
            owner,
            session: client.session.clone(),
            file: client
                .shared_file
                .clone()
                .ok_or_else(|| invalid("physical client requires negotiated backing"))?,
            length: client.shared_length,
            pins: Mutex::new(BTreeMap::new()),
        }))
    }

    /// Whether the current connection has not failed. This is not a liveness
    /// probe and says nothing about the validity of retained page references.
    pub fn connected(&self) -> bool {
        self.client.stream.is_some()
    }

    fn disable(&mut self, reason: &'static str, error: &io::Error) {
        let pending = self.pending.len();
        let outstanding = self.outstanding;
        self.client.stream.take();
        self.client.ready.clear();
        self.pending = VecDeque::new();
        self.outstanding = 0;
        self.retry_at = self.retry_deadline();
        if !self.degraded_reported {
            let unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            eprintln!(
                "pvisor-physical-pool publication_disabled=true accounting_scope=local-pins reason={reason} error_kind={:?} unix_ms={unix_ms} pending_releases={pending} outstanding_references={outstanding}",
                error.kind()
            );
            self.degraded_reported = true;
        }
    }

    fn transport_failed(&self, error: &io::Error) -> bool {
        !self.connected() && error.kind() != io::ErrorKind::InvalidData
    }

    fn retry_deadline(&self) -> Instant {
        let jitter = u64::from(self.current.session[0]) * 1000;
        Instant::now() + self.retry_delay + Duration::from_micros(jitter)
    }

    /// Adopt a separately authenticated and negotiated connection for this
    /// store. Old references remain bound to their old session and file.
    /// Negotiation or capacity failure leaves the current binding unchanged.
    pub fn reconnect(&mut self, client: PoolClient) -> io::Result<()> {
        let current = Self::session(&client, self.owner.clone())?;
        if current.length != self.current.length {
            return Err(invalid("physical recovery backing geometry changed"));
        }
        let old_file = self.current.file.metadata()?;
        let new_file = current.file.metadata()?;
        if old_file.dev() == new_file.dev() && old_file.ino() == new_file.ino() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "mutable pool generation cannot resume an uncertain old session",
            ));
        }
        self.check_recovery_budget()?;
        self.pending = VecDeque::new();
        self.outstanding = 0;
        self.client = client;
        self.current = current;
        self.sessions.push(Arc::downgrade(&self.current));
        self.retry_delay = INITIAL_RETRY;
        self.degraded_reported = false;
        eprintln!("pvisor-physical-pool publication_connected=true accounting_scope=pool");
        Ok(())
    }

    fn check_recovery_budget(&mut self) -> io::Result<()> {
        self.sessions.retain(|session| session.strong_count() != 0);
        let retired = self
            .sessions
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|session| {
                !session
                    .pins
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_empty()
            })
            .count();
        if retired >= MAX_LIVE_SESSIONS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "physical recovery session budget exhausted",
            ));
        }
        Ok(())
    }

    /// Flush deferred releases and, if unavailable, make at most one bounded
    /// recovery attempt after backoff. Must run outside pager locks/barriers.
    pub fn publication_ready(&mut self) -> bool {
        self.flush_releases();
        if self.connected() {
            return true;
        }
        if Instant::now() < self.retry_at {
            return false;
        }
        let result = (|| {
            self.check_recovery_budget()?;
            let path = self
                .endpoint
                .as_ref()
                .ok_or_else(|| io::Error::other("physical recovery endpoint not configured"))?;
            validate_endpoint(path)?;
            let stream = connect_endpoint(path, self.client.timeout)?;
            crate::host_transport::authorize_host_peer_sync(&stream).map_err(io::Error::other)?;
            let mut client = PoolClient::new(stream, self.client.timeout)?;
            client.enable_shared_mapping()?;
            self.reconnect(client)
        })();
        if result.is_err() {
            self.retry_delay = self.retry_delay.saturating_mul(2).min(MAX_RETRY);
            // Per-store jitter keeps a recovering cohort from sharing one timer.
            self.retry_at = self.retry_deadline();
        }
        self.connected()
    }

    fn flush_releases(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let count = self.pending.len().min(DISCOVERY_BATCH);
        let mut tokens = [0; DISCOVERY_BATCH];
        for (slot, token) in tokens[..count].iter_mut().zip(&self.pending) {
            *slot = *token;
        }
        if let Err(error) = self.client.release_tokens(&tokens[..count]) {
            // A release acknowledgement is uncertain, but all these mappings
            // have already been removed. Disconnect keeps remote slots pinned.
            self.disable("release_failed", &error);
            return;
        }
        drop(self.pending.drain(..count));
        self.outstanding -= count;
        if self.outstanding == 0 {
            self.pending = VecDeque::new();
        }
    }

    fn reserve_release_capacity(&mut self, incoming: usize) -> bool {
        debug_assert!(self.pending.len() <= self.outstanding);
        let Some(required) = self.outstanding.checked_add(incoming) else {
            return false;
        };
        self.pending
            .try_reserve(required - self.pending.len())
            .is_ok()
    }

    /// Publish stable snapshots using the existing XXH3/batched protocol.
    /// Reserves release-token space before acquiring references. Reservation
    /// failure returns Limited without new authority or changing live backing.
    /// Transport loss returns Limited for every input and commits no partial
    /// batch. Invalid data remains a fatal outer error; never hide corruption.
    pub fn publish_pages(
        &mut self,
        samples: &[ColdRamSample<'_>],
    ) -> io::Result<Vec<io::Result<Admission<PhysicalReference>>>> {
        if samples.len() > DISCOVERY_BATCH
            || samples
                .iter()
                .any(|sample| sample.bytes.len() != super::super::shared::PAGE)
        {
            return Err(invalid("invalid physical publication geometry"));
        }
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        if !self.publication_ready() {
            return Ok(samples.iter().map(|_| Ok(Admission::Limited)).collect());
        }
        // Capacity is charged to references before publication, not to the
        // rate at which later COW/free reports happen to return them. The
        // publisher's existing reference budget bounds this token bookkeeping.
        if !self.reserve_release_capacity(samples.len()) {
            return Ok(samples.iter().map(|_| Ok(Admission::Limited)).collect());
        }
        let results = match self.client.publish_pages(samples) {
            Ok(results) => results,
            Err(error) if self.transport_failed(&error) => {
                self.disable("publication_failed", &error);
                return Ok(samples.iter().map(|_| Ok(Admission::Limited)).collect());
            }
            Err(error) => return Err(error),
        };
        Ok(results
            .into_iter()
            .map(|result| {
                result.map(|result| match result {
                    Admission::Published(object) => {
                        self.outstanding += 1;
                        let offset = object.offset.expect("physical object").get() - 1;
                        *self
                            .current
                            .pins
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .entry((object.id, offset))
                            .or_default() += 1;
                        Admission::Published(PhysicalReference {
                            backing: self.current.clone(),
                            token: object.token,
                            id: object.id,
                            offset,
                        })
                    }
                    Admission::Observed => Admission::Observed,
                    Admission::Unmatched => Admission::Unmatched,
                    Admission::Limited => Admission::Limited,
                    Admission::Rejected => Admission::Rejected,
                })
            })
            .collect())
    }

    fn owns(&self, object: &PhysicalReference) -> io::Result<()> {
        if !Arc::ptr_eq(&object.backing.owner, &self.owner) {
            return Err(invalid("physical reference belongs to another store"));
        }
        Ok(())
    }

    /// Restore from the exact retained backing with full identity validation.
    /// Performs no publisher IPC or reconnect, including after owner loss.
    pub fn restore(&self, object: &PhysicalReference, output: &mut [u8]) -> io::Result<()> {
        self.owns(object)?;
        if output.len() != super::super::shared::PAGE {
            return Err(invalid("physical restore length mismatch"));
        }
        let bytes = read_shared_page(&object.backing.file, object.backing.length, object.offset)?;
        if identity(&bytes) != object.id {
            return Err(invalid("physical restore checksum mismatch"));
        }
        output.copy_from_slice(&bytes);
        Ok(())
    }

    /// Return this reference's original file, not the current publisher's file.
    pub fn shared_mapping(
        &self,
        object: &PhysicalReference,
    ) -> Option<pvisor_vm::api::SharedRamMapping> {
        self.owns(object).ok()?;
        Some(pvisor_vm::api::SharedRamMapping {
            file: object.backing.file.clone(),
            offset: object.offset,
        })
    }

    /// Consume locally after mapping dependencies are removed. Never performs
    /// IPC or grows the token queue: publication has already reserved its space.
    /// Notifications flush in bounded frames only at publication/readiness/stats.
    pub fn release_objects(&mut self, objects: Vec<PhysicalReference>) -> io::Result<()> {
        for object in &objects {
            self.owns(object)?;
        }
        for object in objects {
            if self.connected() && Arc::ptr_eq(&object.backing, &self.current) {
                debug_assert!(self.pending.len() < self.outstanding);
                debug_assert!(self.pending.len() < self.pending.capacity());
                self.pending.push_back(object.token);
            }
        }
        Ok(())
    }

    /// Drain hints from the current publisher only; recovery drops old hints.
    pub fn take_ready(&mut self) -> Vec<u64> {
        self.client.take_ready()
    }

    /// Return live pool accounting, or local explicit-pin accounting after
    /// transport loss. Local counts exclude other VMs and unknown remote pins;
    /// `connected()` and the transition log identify the accounting scope.
    /// Connected counts include queued releases until acknowledged by the pool.
    pub fn stats(&mut self) -> io::Result<PoolStats> {
        self.flush_releases();
        if self.connected() {
            match self.client.stats() {
                Ok(stats) => return Ok(stats),
                Err(error) if self.transport_failed(&error) => self.disable("stats_failed", &error),
                Err(error) => return Err(error),
            }
        }
        self.sessions.retain(|session| session.strong_count() != 0);
        let mut objects = 0;
        let mut references = 0;
        for session in self.sessions.iter().filter_map(Weak::upgrade) {
            let pins = session.pins.lock().unwrap_or_else(|e| e.into_inner());
            objects += pins.len() as u64;
            references += pins.values().map(|&count| count as u64).sum::<u64>();
        }
        Ok(PoolStats {
            encoded_bytes: objects * super::super::shared::PAGE as u64,
            objects,
            session_references: references,
            cross_session_objects: 0,
        })
    }
}

fn validate_endpoint(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("physical recovery endpoint has no parent"))?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if !path.is_absolute()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "physical recovery endpoint must have a private same-user parent",
        ));
    }
    Ok(())
}

// std's blocking Unix connect can wait indefinitely on a full accept backlog.
// Recovery is optional work: a full backlog is an immediate retryable rejection.
fn connect_endpoint(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    use std::os::{
        fd::{FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    };
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "zero connect timeout",
        ));
    }
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as _;
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            address.sun_path.as_mut_ptr().cast(),
            bytes.len(),
        );
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = length as _;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "macos")]
    {
        let enabled: libc::c_int = 1;
        if unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&enabled as *const libc::c_int).cast(),
                std::mem::size_of_val(&enabled) as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let connected = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length as _,
        )
    };
    let error = (connected != 0).then(io::Error::last_os_error);
    let mut stream = UnixStream::from(fd);
    if connected == 0 {
        return Ok(stream);
    }
    let error = error.unwrap();
    if !matches!(
        error.raw_os_error(),
        Some(libc::EINPROGRESS | libc::EALREADY)
    ) {
        return Err(error);
    }
    let mut deadline = DeadlineStream::new(&mut stream, timeout)?;
    deadline.io(libc::POLLOUT, |stream| {
        let mut error: libc::c_int = 0;
        let mut length = std::mem::size_of_val(&error) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut libc::c_int).cast(),
                &mut length,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if matches!(error, libc::EINPROGRESS | libc::EALREADY) {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        if stream.peer_addr().is_err() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        Ok(())
    })?;
    Ok(stream)
}

impl pvisor_vm::api::ColdRamStore for PhysicalClient {
    type Object = PhysicalReference;

    fn publication_ready(&mut self) -> bool {
        Self::publication_ready(self)
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<Self::Object> {
        let sample = ColdRamSample { page: 0, bytes };
        match self.publish_pages(&[sample])?.pop().unwrap()? {
            Admission::Published(object) => Ok(object),
            _ => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "physical page not admitted",
            )),
        }
    }

    fn put_batch(
        &mut self,
        blocks: &[ColdRamSample<'_>],
    ) -> io::Result<Vec<io::Result<Admission<Self::Object>>>> {
        self.publish_pages(blocks)
    }

    fn restore(&mut self, object: &Self::Object, output: &mut [u8]) -> io::Result<()> {
        Self::restore(self, object, output)
    }

    fn release(&mut self, object: Self::Object) -> io::Result<()> {
        self.release_objects(vec![object])
    }

    fn release_batch(&mut self, objects: Vec<Self::Object>) -> io::Result<()> {
        self.release_objects(objects)
    }

    fn take_ready(&mut self) -> Vec<u64> {
        Self::take_ready(self)
    }

    fn shared_mapping(&self, object: &Self::Object) -> Option<pvisor_vm::api::SharedRamMapping> {
        Self::shared_mapping(self, object)
    }

    fn stats(&mut self) -> io::Result<pvisor_vm::api::ColdRamPoolStats> {
        let stats = Self::stats(self)?;
        Ok(stats.into_cold(if self.connected() {
            pvisor_vm::api::ColdRamAccountingScope::Pool
        } else {
            pvisor_vm::api::ColdRamAccountingScope::LocalPins
        }))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::ram_backing::shared::{PAGE, SharedPool};
    use std::{
        os::{fd::AsRawFd, unix::net::UnixListener},
        process::{Child, Command, Stdio},
    };

    const CHILD_TEST: &str = "ram_backing::ipc::physical_client::tests::physical_service_child";
    const SOCKET_ENV: &str = "PVISOR_TEST_PHYSICAL_SOCKET";

    #[test]
    fn physical_service_child() {
        let Some(path) = std::env::var_os(SOCKET_ENV) else {
            return;
        };
        let fill: u8 = std::env::var("PVISOR_TEST_PHYSICAL_FILL")
            .unwrap()
            .parse()
            .unwrap();
        let pool = Arc::new(Mutex::new(SharedPool::new(PAGE * 4, 4).unwrap()));
        let _keep = pool.lock().unwrap().intern(&vec![fill; PAGE]).unwrap();
        let listener = UnixListener::bind(path).unwrap();
        for stream in listener.incoming() {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let _ = serve_shared(stream.unwrap(), pool, 32768);
            });
        }
    }

    struct Service {
        child: Child,
        socket: PathBuf,
        _directory: tempfile::TempDir,
    }

    impl Service {
        fn start(fill: u8) -> Self {
            let directory = tempfile::tempdir().unwrap();
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let socket = directory.path().join("p");
            let child = Self::spawn(&socket, fill);
            Self {
                child,
                socket,
                _directory: directory,
            }
        }

        fn spawn(socket: &Path, fill: u8) -> Child {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CHILD_TEST, "--nocapture"])
                .env(SOCKET_ENV, socket)
                .env("PVISOR_TEST_PHYSICAL_FILL", fill.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap()
        }

        fn restart(&mut self, fill: u8) {
            self.stop();
            std::fs::remove_file(&self.socket).unwrap();
            self.child = Self::spawn(&self.socket, fill);
            drop(self.connect());
        }

        fn connect(&mut self) -> PoolClient {
            let until = Instant::now() + Duration::from_secs(5);
            let stream = loop {
                if let Ok(stream) = UnixStream::connect(&self.socket) {
                    break stream;
                }
                assert!(self.child.try_wait().unwrap().is_none(), "service exited");
                assert!(Instant::now() < until, "service startup timeout");
                std::thread::sleep(Duration::from_millis(5));
            };
            let mut client = PoolClient::new(stream, Duration::from_millis(500)).unwrap();
            client.enable_shared_mapping().unwrap();
            client
        }

        fn stop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Drop for Service {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn published(client: &mut PhysicalClient, fill: u8) -> PhysicalReference {
        let bytes = vec![fill; PAGE];
        let sample = ColdRamSample {
            page: 1,
            bytes: &bytes,
        };
        match client
            .publish_pages(&[sample])
            .unwrap()
            .pop()
            .unwrap()
            .unwrap()
        {
            Admission::Published(object) => object,
            _ => panic!("preseeded content was not published"),
        }
    }

    struct Mapping(*mut libc::c_void);
    impl Mapping {
        fn new(mapping: pvisor_vm::api::SharedRamMapping) -> Self {
            let address = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    PAGE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE,
                    mapping.file.as_raw_fd(),
                    mapping.offset as libc::off_t,
                )
            };
            assert_ne!(address, libc::MAP_FAILED);
            Self(address)
        }
        fn first(&self) -> u8 {
            unsafe { std::ptr::read_volatile(self.0.cast::<u8>()) }
        }
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.0, PAGE);
            }
        }
    }

    #[test]
    fn physical_backing_refault_cow_and_local_restore_survive_real_owner_death() {
        let mut service = Service::start(0x37);
        let mut client = PhysicalClient::new(service.connect(), None).unwrap();
        let object = published(&mut client, 0x37);
        let mapping = Mapping::new(client.shared_mapping(&object).unwrap());
        assert_eq!(mapping.first(), 0x37);
        service.stop();
        let stable = vec![0x37; PAGE];
        assert!(matches!(
            client
                .publish_pages(&[ColdRamSample {
                    page: 2,
                    bytes: &stable,
                }])
                .unwrap()
                .pop()
                .unwrap()
                .unwrap(),
            Admission::Limited
        ));
        let stats = client.stats().unwrap();
        assert!(!client.connected());
        assert_eq!(stats.objects, 1);
        assert_eq!(stats.session_references, 1);
        assert_eq!(
            unsafe { libc::madvise(mapping.0, PAGE, libc::MADV_DONTNEED) },
            0
        );
        assert_eq!(mapping.first(), 0x37, "a post-death file refault must work");
        unsafe {
            std::ptr::write_volatile(mapping.0.cast::<u8>(), 0x72);
        }
        assert_eq!(mapping.first(), 0x72);
        let mut original = vec![0; PAGE];
        client.restore(&object, &mut original).unwrap();
        assert_eq!(original, vec![0x37; PAGE]);
        let samples = [ColdRamSample {
            page: 2,
            bytes: &original,
        }];
        assert!(matches!(
            client
                .publish_pages(&samples)
                .unwrap()
                .pop()
                .unwrap()
                .unwrap(),
            Admission::Limited
        ));
        drop(mapping);
        client.release_objects(vec![object]).unwrap();
        assert_eq!(client.stats().unwrap().session_references, 0);
        assert!(std::mem::size_of::<PhysicalReference>() <= 64);
    }

    #[test]
    fn physical_reconnect_keeps_old_file_authority_and_never_releases_old_tokens() {
        let mut old_service = Service::start(0x37);
        let mut client = PhysicalClient::new(old_service.connect(), None).unwrap();
        let old = published(&mut client, 0x37);
        let old_mapping = Mapping::new(client.shared_mapping(&old).unwrap());
        let old_inode = old.backing.file.metadata().unwrap().ino();
        old_service.stop();
        client.stats().unwrap();
        let mut replacement = Service::start(0x72);
        client.reconnect(replacement.connect()).unwrap();
        let new = published(&mut client, 0x72);
        assert_ne!(new.backing.file.metadata().unwrap().ino(), old_inode);
        assert_eq!(
            client
                .shared_mapping(&old)
                .unwrap()
                .file
                .metadata()
                .unwrap()
                .ino(),
            old_inode
        );
        assert_eq!(old_mapping.first(), 0x37);
        let mut bytes = vec![0; PAGE];
        client.restore(&old, &mut bytes).unwrap();
        assert_eq!(bytes, vec![0x37; PAGE]);
        let new_mapping = Mapping::new(client.shared_mapping(&new).unwrap());
        assert_eq!(new_mapping.first(), 0x72);
        drop(old_mapping);
        client.release_objects(vec![old]).unwrap();
        assert_eq!(client.stats().unwrap().session_references, 1);
        drop(new_mapping);
        client.release_objects(vec![new]).unwrap();
        assert_eq!(client.stats().unwrap().session_references, 0);
    }

    #[test]
    fn physical_release_has_no_rpc_and_cannot_be_rebound_to_another_store() {
        let mut service = Service::start(9);
        let mut a = PhysicalClient::new(service.connect(), None).unwrap();
        let mut b = PhysicalClient::new(service.connect(), None).unwrap();
        let object = published(&mut a, 9);
        assert!(b.shared_mapping(&object).is_none());
        let mut untouched = vec![0x71; PAGE];
        assert!(b.restore(&object, &mut untouched).is_err());
        assert_eq!(untouched, vec![0x71; PAGE]);
        let old_connection = a.client.stream.take().unwrap();
        old_connection.shutdown(std::net::Shutdown::Both).unwrap();
        a.release_objects(vec![object]).unwrap();
        assert_eq!(a.pending.len(), 0);
        assert_eq!(a.stats().unwrap().session_references, 0);
        b.stats().unwrap();
    }

    #[test]
    fn physical_reconnect_rejects_the_same_mutable_generation() {
        let mut service = Service::start(9);
        let mut client = PhysicalClient::new(service.connect(), None).unwrap();
        let object = published(&mut client, 9);
        assert_eq!(
            client.reconnect(service.connect()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let mut bytes = vec![0; PAGE];
        client.restore(&object, &mut bytes).unwrap();
        assert_eq!(bytes, vec![9; PAGE]);
        client.release_objects(vec![object]).unwrap();
        client.stats().unwrap();
    }

    #[test]
    fn connected_release_is_deferred_until_optional_store_work() {
        let mut service = Service::start(9);
        let mut client = PhysicalClient::new(service.connect(), None).unwrap();
        let object = published(&mut client, 9);
        client.release_objects(vec![object]).unwrap();
        assert_eq!(client.pending.len(), 1);
        // Bypass the adapter's flush to inspect the publisher's untouched lease.
        assert_eq!(client.client.stats().unwrap().session_references, 1);
        assert_eq!(client.stats().unwrap().session_references, 0);
        assert!(client.pending.is_empty());
    }

    #[test]
    fn cow_release_bursts_do_not_disconnect_a_healthy_publisher() {
        check_cow_release_bursts(4096);
    }

    #[test]
    fn a_whole_payload_can_be_released_without_rpc_or_queue_growth() {
        check_cow_release_bursts(16 * 1024);
    }

    fn check_cow_release_bursts(maintenance_batch: usize) {
        const REFERENCES: usize = 16 * 1024; // One 64 MiB Linux payload.
        let mut service = Service::start(9);
        let mut client = PhysicalClient::new(service.connect(), None).unwrap();
        let live = published(&mut client, 9);
        let mapping = Mapping::new(client.shared_mapping(&live).unwrap());
        let bytes = vec![9; PAGE];
        let mut retired = Vec::new();
        for first in (0..REFERENCES).step_by(DISCOVERY_BATCH) {
            let samples: Vec<_> = (first..first + DISCOVERY_BATCH)
                .map(|page| ColdRamSample {
                    page: page as u64 + 2,
                    bytes: &bytes,
                })
                .collect();
            for result in client.publish_pages(&samples).unwrap() {
                match result.unwrap() {
                    Admission::Published(object) => retired.push(object),
                    _ => panic!("preseeded page was not published"),
                }
            }
        }
        assert_eq!(retired.len(), REFERENCES);
        assert!(client.pending.capacity() >= client.outstanding);
        let mut retired = retired.into_iter();
        loop {
            let batch: Vec<_> = retired.by_ref().take(maintenance_batch).collect();
            if batch.is_empty() {
                break;
            }
            let remote = client.client.stats().unwrap().session_references;
            let capacity = client.pending.capacity();
            client.release_objects(batch).unwrap();
            assert!(service.child.try_wait().unwrap().is_none());
            assert!(
                client.connected(),
                "local COW release backlog must not disconnect a healthy publisher"
            );
            assert_eq!(
                client.client.stats().unwrap().session_references,
                remote,
                "mandatory local release must perform no RPC"
            );
            assert_eq!(client.outstanding as u64, remote);
            assert_eq!(
                client.pending.capacity(),
                capacity,
                "release must not allocate"
            );
            let queued = client.pending.len();
            assert!(client.publication_ready());
            assert_eq!(client.pending.len(), queued - queued.min(DISCOVERY_BATCH));
        }
        while !client.pending.is_empty() {
            let queued = client.pending.len();
            assert!(client.publication_ready());
            assert_eq!(client.pending.len(), queued - queued.min(DISCOVERY_BATCH));
        }
        assert_eq!(client.client.stats().unwrap().session_references, 1);
        assert_eq!(mapping.first(), 9);
        let fresh = published(&mut client, 9);
        let mut restored = vec![0; PAGE];
        client.restore(&live, &mut restored).unwrap();
        assert_eq!(restored, bytes);
        client.restore(&fresh, &mut restored).unwrap();
        assert_eq!(restored, bytes);
        drop(mapping);
        client.release_objects(vec![live, fresh]).unwrap();
        assert_eq!(client.stats().unwrap().session_references, 0);
        assert_eq!(client.outstanding, 0);
        assert_eq!(client.pending.capacity(), 0);
    }

    #[test]
    fn release_bookkeeping_reservation_failure_preserves_live_authority() {
        let mut service = Service::start(9);
        let mut client = PhysicalClient::new(service.connect(), None).unwrap();
        let live = published(&mut client, 9);
        let mapping = Mapping::new(client.shared_mapping(&live).unwrap());
        let capacity = client.pending.capacity();
        // Capacity overflow is deterministic and never attempts a huge allocation.
        let impossible = isize::MAX as usize / std::mem::size_of::<u64>() + 1;
        assert!(!client.reserve_release_capacity(impossible));
        assert!(!client.reserve_release_capacity(usize::MAX));
        assert!(client.connected());
        assert_eq!(client.outstanding, 1);
        assert!(client.pending.is_empty());
        assert_eq!(client.pending.capacity(), capacity);
        assert_eq!(client.client.stats().unwrap().session_references, 1);
        assert_eq!(mapping.first(), 9);
        drop(mapping);
        client.release_objects(vec![live]).unwrap();
        assert_eq!(client.stats().unwrap().session_references, 0);
    }

    #[test]
    fn release_transport_failure_cannot_replay_reserved_tokens_on_a_new_generation() {
        let mut service = Service::start(9);
        let mut client = PhysicalClient::new(service.connect(), None).unwrap();
        let old = published(&mut client, 9);
        let retired = published(&mut client, 9);
        client.release_objects(vec![retired]).unwrap();
        assert_eq!(client.pending.len(), 1);
        service.stop();
        assert!(!client.publication_ready());
        assert_eq!(client.outstanding, 0);
        assert!(client.pending.is_empty());
        let mut replacement = Service::start(7);
        client.reconnect(replacement.connect()).unwrap();
        let new = published(&mut client, 7);
        let mut bytes = vec![0; PAGE];
        client.restore(&old, &mut bytes).unwrap();
        assert_eq!(bytes, vec![9; PAGE]);
        client.release_objects(vec![old]).unwrap();
        assert_eq!(client.stats().unwrap().session_references, 1);
        assert_eq!(client.outstanding, 1);
        client.restore(&new, &mut bytes).unwrap();
        assert_eq!(bytes, vec![7; PAGE]);
        client.release_objects(vec![new]).unwrap();
        assert_eq!(client.stats().unwrap().session_references, 0);
    }

    #[test]
    fn corrupt_physical_content_is_not_downgraded_to_unavailability() {
        use std::os::unix::fs::FileExt;
        let mut service = Service::start(9);
        let mut client = PhysicalClient::new(service.connect(), None).unwrap();
        let writable = std::fs::OpenOptions::new()
            .write(true)
            .open(format!("/proc/self/fd/{}", client.current.file.as_raw_fd()))
            .unwrap();
        writable.write_all_at(&[8], 0).unwrap();
        let bytes = vec![9; PAGE];
        assert_eq!(
            client
                .publish_pages(&[ColdRamSample {
                    page: 1,
                    bytes: &bytes
                }])
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(!client.connected());
    }

    #[test]
    fn live_backing_recovery_budget_is_bounded_and_released_locally() {
        let mut service = Service::start(9);
        let mut client = PhysicalClient::new(service.connect(), None).unwrap();
        let mut objects = vec![published(&mut client, 9)];
        for _ in 1..MAX_LIVE_SESSIONS {
            service.stop();
            client.stats().unwrap();
            service = Service::start(9);
            client.reconnect(service.connect()).unwrap();
            objects.push(published(&mut client, 9));
        }
        service.stop();
        client.stats().unwrap();
        assert_eq!(client.stats().unwrap().objects, MAX_LIVE_SESSIONS as u64);
        service = Service::start(9);
        assert_eq!(
            client.reconnect(service.connect()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        client.release_objects(objects).unwrap();
        assert_eq!(client.stats().unwrap().objects, 0);
        client.reconnect(service.connect()).unwrap();
        assert!(client.connected());
    }

    #[test]
    fn endpoint_recovery_reauthenticates_and_preserves_the_old_generation() {
        let mut service = Service::start(9);
        let mut client =
            PhysicalClient::new(service.connect(), Some(service.socket.clone())).unwrap();
        let old = published(&mut client, 9);
        service.restart(7);
        client.stats().unwrap();
        assert!(!client.connected());
        client.retry_at = Instant::now() + MAX_RETRY;
        assert!(
            !client.publication_ready(),
            "recovery must respect its backoff"
        );
        client.retry_at = Instant::now() - Duration::from_secs(1);
        assert!(client.publication_ready());
        let new = published(&mut client, 7);
        assert_ne!(old.backing.session, new.backing.session);
        let mut bytes = vec![0; PAGE];
        client.restore(&old, &mut bytes).unwrap();
        assert_eq!(bytes, vec![9; PAGE]);
        client.restore(&new, &mut bytes).unwrap();
        assert_eq!(bytes, vec![7; PAGE]);
        client.release_objects(vec![old, new]).unwrap();
        assert_eq!(client.stats().unwrap().session_references, 0);
    }

    #[test]
    fn unsafe_recovery_parent_cannot_replace_existing_backing_authority() {
        let mut service = Service::start(9);
        let mut client =
            PhysicalClient::new(service.connect(), Some(service.socket.clone())).unwrap();
        let object = published(&mut client, 9);
        service.stop();
        client.stats().unwrap();
        std::fs::set_permissions(
            service.socket.parent().unwrap(),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        client.retry_at = Instant::now() - Duration::from_secs(1);
        assert!(!client.publication_ready());
        let mut bytes = vec![0; PAGE];
        client.restore(&object, &mut bytes).unwrap();
        assert_eq!(bytes, vec![9; PAGE]);
        client.release_objects(vec![object]).unwrap();
    }

    #[test]
    fn recovery_connect_does_not_block_on_a_full_listener_backlog() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("p");
        let listener = UnixListener::bind(&socket).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let _queued = UnixStream::connect(&socket).unwrap();
        let started = Instant::now();
        let result = connect_endpoint(&socket, Duration::from_millis(50));
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
