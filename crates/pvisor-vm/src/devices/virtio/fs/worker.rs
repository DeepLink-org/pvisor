#[cfg(target_os = "macos")]
use crate::utils::worker_message::WorkerMessage;
#[cfg(target_os = "macos")]
use crossbeam_channel::Sender;

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
#[cfg(test)]
use std::time::Instant;

use crate::utils::epoll::{ControlOperation, Epoll, EpollEvent, EventSet};
use crate::utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::GuestMemoryMmap;

use super::super::Queue;
use super::augment_fs::AugmentFs;
use super::defs::{HPQ_INDEX, REQ_INDEX};
use super::descriptor_utils::{OwnedDescriptorChain, Reader, Writer};
#[cfg(test)]
use super::fuse::ReadIn;
use super::fuse::{InHeader, Opcode};
use super::inode_alloc::InodeAllocator;
use super::null_fs::NullFs;
use super::overlay::{Config as OverlayConfig, OverlayFs};
use super::passthrough::{self, PassthroughFs};
use super::read_only::PassthroughFsRo;
use super::server::Server;
use super::virtual_entry::VirtualDirEntry;
use super::write_buffer::WriteBufferOptions;
use crate::devices::virtio::{InterruptTransport, VirtioShmRegion};

mod scheduler;

enum FsServer {
    ReadWrite(Server<AugmentFs<PassthroughFs>>),
    ReadOnly(Server<AugmentFs<PassthroughFsRo>>),
    Null(Server<AugmentFs<NullFs>>),
    Overlay(Box<Server<AugmentFs<OverlayFs>>>),
}

impl FsServer {
    fn configure_profile_scope(&mut self, scope: &str) {
        match self {
            Self::ReadWrite(server) => server.configure_profile_scope(scope),
            Self::ReadOnly(server) => server.configure_profile_scope(scope),
            Self::Null(server) => server.configure_profile_scope(scope),
            Self::Overlay(server) => server.configure_profile_scope(scope),
        }
    }

    fn dispatch_profile(&self) -> pvisor_overlay_core::profile::Profile {
        match self {
            Self::ReadWrite(server) => server.dispatch_profile(),
            Self::ReadOnly(server) => server.dispatch_profile(),
            Self::Null(server) => server.dispatch_profile(),
            Self::Overlay(server) => server.dispatch_profile(),
        }
    }

    fn handle_message(
        &self,
        r: (InHeader, Reader),
        w: Writer,
        allow_idmap: bool,
        shm_region: &Option<VirtioShmRegion>,
        exit_code: &Arc<AtomicI32>,
        #[cfg(target_os = "macos")] map_sender: &Option<Sender<WorkerMessage>>,
    ) -> super::Result<usize> {
        match self {
            FsServer::ReadWrite(s) => s.handle_message(
                r,
                w,
                allow_idmap,
                shm_region,
                exit_code,
                #[cfg(target_os = "macos")]
                map_sender,
            ),
            FsServer::ReadOnly(s) => s.handle_message(
                r,
                w,
                allow_idmap,
                shm_region,
                exit_code,
                #[cfg(target_os = "macos")]
                map_sender,
            ),
            FsServer::Null(s) => s.handle_message(
                r,
                w,
                allow_idmap,
                shm_region,
                exit_code,
                #[cfg(target_os = "macos")]
                map_sender,
            ),
            FsServer::Overlay(s) => s.handle_message(
                r,
                w,
                allow_idmap,
                shm_region,
                exit_code,
                #[cfg(target_os = "macos")]
                map_sender,
            ),
        }
    }
}

pub struct FsWorker {
    queues: Vec<Queue>,
    queue_evts: Vec<Arc<EventFd>>,
    interrupt: InterruptTransport,
    execution: Arc<Execution>,
    dispatch_profile: pvisor_overlay_core::profile::Profile,
    inode_alloc: Arc<InodeAllocator>,
    stop_fd: EventFd,
}

/// Shared execution state, never virtqueue indices. The session guard also
/// protects INIT/DESTROY against a driver issuing overlapping control requests.
struct Execution {
    mem: GuestMemoryMmap,
    server: FsServer,
    session: RwLock<()>,
    allow_idmap: bool,
    shm_region: Option<VirtioShmRegion>,
    exit_code: Arc<AtomicI32>,
    #[cfg(target_os = "macos")]
    map_sender: Option<Sender<WorkerMessage>>,
}

impl Execution {
    fn handle(&self, request: &Request) -> usize {
        let decoded = request
            .buffers
            // The owner captured the header exactly once. Skip its guest bytes
            // without constructing or decoding another header reader.
            .reader_writer_after(&self.mem, std::mem::size_of::<InHeader>());
        match decoded {
            Ok((reader, writer)) => self.handle_decoded(request.header, reader, writer),
            Err(error) => {
                error!("invalid filesystem buffers: {error}");
                0
            }
        }
    }

    fn handle_decoded(&self, header: InHeader, reader: Reader, writer: Writer) -> usize {
        let phase_checkpoint = header.opcode == Opcode::Ioctl as u32
            && reader
                .clone()
                .read_obj::<super::fuse::IoctlIn>()
                .is_ok_and(|ioctl| ioctl.cmd == super::server::PROFILE_PHASE_IOCTL);
        let run = || {
            self.server.handle_message(
                (header, reader),
                writer,
                self.allow_idmap,
                &self.shm_region,
                &self.exit_code,
                #[cfg(target_os = "macos")]
                &self.map_sender,
            )
        };
        let result = if header.opcode == Opcode::Init as u32
            || header.opcode == Opcode::Destroy as u32
            || phase_checkpoint
        {
            let _session = self.session.write().expect("FUSE session poisoned");
            run()
        } else {
            let _session = self.session.read().expect("FUSE session poisoned");
            run()
        };
        match result {
            Ok(len) => len,
            Err(error) => {
                error!("error handling filesystem request: {error:?}");
                0
            }
        }
    }
}

// The old pool is retained only as a test fixture for the existing descriptor,
// memory-lease and wakeup regressions during this scheduling migration. The
// production worker exclusively uses scheduler::serve below.
#[cfg(test)]
fn parallel_candidate(header: &InHeader, body: &Reader) -> bool {
    match header.opcode {
        x if x == Opcode::Read as u32 => body
            .clone()
            .read_obj::<ReadIn>()
            .is_ok_and(|read| read.size >= 64 * 1024),
        x if x == Opcode::Opendir as u32
            || x == Opcode::Readdir as u32
            || x == Opcode::Readdirplus as u32 =>
        {
            true
        }
        _ => false,
    }
}

struct Request {
    queue: usize,
    index: u16,
    header: InHeader,
    buffers: OwnedDescriptorChain,
    // Legacy pool test diagnostics; queue-local execution reads no clock here.
    #[cfg(test)]
    profile_timestamp: Option<Instant>,
}

#[cfg(test)]
struct Completion {
    // Keep both captured descriptors and the RAM lease until add_used finishes.
    request: Request,
    len: usize,
}

/// A per-device bounded blocking-I/O pool. Completion is asynchronous to the
/// queue owner; the existing pread/pwrite implementation remains zero-copy.
/// No borrowed VolatileSlice or unsafe Send implementation crosses threads.
#[cfg(test)]
struct RequestPool {
    jobs: Option<crossbeam_channel::Sender<Request>>,
    idle_jobs: Option<crossbeam_channel::Sender<Request>>,
    completed: crossbeam_channel::Receiver<Completion>,
    wake: Arc<EventFd>,
    awaiting_wake: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
    in_flight: usize,
    limit: usize,
    profile: pvisor_overlay_core::profile::Profile,
}

#[cfg(test)]
impl RequestPool {
    fn new(
        handle: impl Fn(&Request) -> usize + Send + Sync + 'static,
        workers: usize,
        limit: usize,
        profile: pvisor_overlay_core::profile::Profile,
    ) -> io::Result<Self> {
        if workers == 0 || limit == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty filesystem pool",
            ));
        }
        let handle = Arc::new(handle);
        let (jobs, receive) = crossbeam_channel::bounded::<Request>(limit);
        // Keep isolated requests on one warm worker. Bursts still use the
        // shared FIFO; this slot counts against the same in-flight bound.
        let (idle_jobs, idle_receive) = crossbeam_channel::bounded::<Request>(1);
        // Although this channel is unbounded, at most `limit` accepted requests
        // can exist, including queued, executing and unpublished completions.
        let (completed, completions) = crossbeam_channel::unbounded();
        let wake = Arc::new(EventFd::new(EFD_NONBLOCK)?);
        let mut pool = Self {
            jobs: Some(jobs),
            idle_jobs: Some(idle_jobs),
            completed: completions,
            wake,
            awaiting_wake: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            in_flight: 0,
            limit,
            profile,
        };
        for id in 0..workers {
            let receive = receive.clone();
            let idle_receive = idle_receive.clone();
            let completed = completed.clone();
            let wake = pool.wake.clone();
            let awaiting_wake = pool.awaiting_wake.clone();
            let handle = handle.clone();
            let profile = pool.profile.clone();
            pool.threads
                .push(
                    thread::Builder::new()
                        .name(format!("fs io {id}"))
                        .spawn(move || {
                            loop {
                                let received = if id == 0 {
                                    crossbeam_channel::select_biased! {
                                        recv(idle_receive) -> result => result.or_else(|_| receive.recv()),
                                        recv(receive) -> result => result,
                                    }
                                } else {
                                    receive.recv()
                                };
                                let Ok(mut request) = received else { break };
                                profile.record_since("pool_queue_wait", request.profile_timestamp);
                                let service = profile.span("pool_service");
                                // A lost worker would make freeze wait forever. Fail the
                                // isolated VMM rather than publish a successful snapshot.
                                let len =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        handle(&request)
                                    }))
                                    .unwrap_or_else(|_| {
                                        error!("filesystem request worker panicked");
                                        std::process::abort();
                                    });
                                drop(service);
                                request.profile_timestamp = profile.timestamp();
                                if completed.send(Completion { request, len }).is_err() {
                                    break;
                                }
                                if awaiting_wake.swap(false, Ordering::AcqRel) {
                                    if let Err(error) = wake.write(1) {
                                        error!("filesystem completion wake failed: {error}");
                                        std::process::abort();
                                    }
                                }
                            }
                        })?,
                );
        }
        Ok(pool)
    }

    fn wait_timeout(&self) -> i32 {
        // Pair the owner's arm with the worker's disarm after channel send.
        // If completion wins, acquire makes it visible before the empty check;
        // if arm wins, that worker signals the sleeping owner. Never park with
        // an already queued completion, but still poll stop/kick events first.
        self.awaiting_wake.swap(true, Ordering::AcqRel);
        if self.completed.is_empty() {
            -1
        } else {
            0
        }
    }

    fn submit(&mut self, mut request: Request) {
        request.profile_timestamp = self.profile.timestamp();
        assert!(self.in_flight < self.limit);
        // Capacity covers every in-flight request, so the owner never blocks
        // behind I/O and can always service the dedicated high-priority queue.
        let sender = if self.in_flight == 0 {
            &self.idle_jobs
        } else {
            &self.jobs
        };
        sender
            .as_ref()
            .expect("filesystem pool closed")
            .try_send(request)
            .unwrap_or_else(|_| panic!("filesystem pool capacity invariant violated"));
        self.in_flight += 1;
    }
}

#[cfg(test)]
impl Drop for RequestPool {
    fn drop(&mut self) {
        self.jobs.take();
        self.idle_jobs.take();
        for worker in self.threads.drain(..) {
            if worker.join().is_err() {
                std::process::abort();
            }
        }
    }
}

pub(super) fn worker_count() -> usize {
    // Controls normal queue count; hiprio always gets its own lane. The guest
    // may initialize fewer queues, for example on a single-vCPU VM.
    std::env::var("PVISOR_VM_FS_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| (1..=8).contains(count))
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, |n| n.get().min(4)))
}

fn root_data_cache_enabled(tag: &str, setting: Option<&str>) -> io::Result<bool> {
    match setting {
        None | Some("root-data") => Ok(tag == "/dev/root"),
        Some("off") => Ok(false),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_VM_CACHE_EXPERIMENT supports root-data or off; metadata and no-open modes are not integrated",
        )),
    }
}

fn guest_cache_enabled(setting: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match setting {
        None => Ok(true),
        Some(value) if value == "1" => Ok(true),
        Some(value) if value == "0" => Ok(false),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_VM_GUEST_CACHE must be 0 or 1",
        )),
    }
}

fn host_writeback_enabled(setting: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match setting {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_VM_FS_HOST_WRITEBACK must be 0 or 1",
        )),
    }
}

/// Private engineering controls. Values never enter the public VM contract or
/// snapshots; each new connection validates them before admitting any request.
#[derive(Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HostWritebackConfig {
    capacity_mib: usize,
    jobs: usize,
    batch_kib: usize,
    chunks: usize,
    delay_us: u64,
    qualification_lock: bool,
    synchronous: bool,
}

impl Default for HostWritebackConfig {
    fn default() -> Self {
        Self {
            capacity_mib: 64,
            jobs: 1024,
            batch_kib: 1024,
            chunks: 64,
            delay_us: 10_000,
            qualification_lock: false,
            synchronous: false,
        }
    }
}

impl HostWritebackConfig {
    fn options(&self) -> io::Result<WriteBufferOptions> {
        let invalid =
            || io::Error::new(io::ErrorKind::InvalidInput, "host writeback size overflow");
        let options = WriteBufferOptions {
            budget_bytes: self
                .capacity_mib
                .checked_mul(1024 * 1024)
                .ok_or_else(invalid)?,
            max_jobs: self.jobs,
            batch_bytes: self.batch_kib.checked_mul(1024).ok_or_else(invalid)?,
            max_chunks: self.chunks,
            max_delay: std::time::Duration::from_micros(self.delay_us),
        };
        options.validate()?;
        Ok(options)
    }
}

fn host_writeback_config(setting: Option<&std::ffi::OsStr>) -> io::Result<HostWritebackConfig> {
    let config = match setting {
        None => HostWritebackConfig::default(),
        Some(value) => {
            let invalid = |error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("PVISOR_VM_FS_HOST_WRITEBACK_CONFIG: {error}"),
                )
            };
            let parsed: serde_json::Value =
                serde_json::from_slice(value.as_bytes()).map_err(invalid)?;
            if !parsed.is_object() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "PVISOR_VM_FS_HOST_WRITEBACK_CONFIG must be a JSON object",
                ));
            }
            // Decode the original bytes so duplicate fields are also rejected.
            serde_json::from_slice(value.as_bytes()).map_err(invalid)?
        }
    };
    config.options()?;
    Ok(config)
}

fn exclusive_upper_absence_cache_enabled(setting: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match setting {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE must be 0 or 1",
        )),
    }
}

fn readonly_flush_elision_enabled(setting: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match setting {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_VM_FS_READONLY_FLUSH_ELISION must be 0 or 1",
        )),
    }
}

fn lower_data_cache_enabled(setting: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match setting {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_VM_FS_LOWER_DATA_CACHE must be 0 or 1",
        )),
    }
}

fn negative_entry_cache_enabled(setting: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match setting {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE must be 0 or 1",
        )),
    }
}

fn directory_cache_capacity(setting: Option<&std::ffi::OsStr>) -> io::Result<usize> {
    let Some(setting) = setting else {
        return Ok(super::overlay::DEFAULT_DIRECTORY_CACHE_CAPACITY);
    };
    setting
        .to_str()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|capacity| (1..=super::overlay::MAX_DIRECTORY_CACHE_CAPACITY).contains(capacity))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "PVISOR_VM_FS_DIRECTORY_CACHE_CAPACITY must be an ASCII decimal integer from 1 to 4096",
            )
        })
}

pub(super) struct FsWorkerConfig {
    pub fs_tag: String,
    pub queues: Vec<Queue>,
    pub queue_evts: Vec<Arc<EventFd>>,
    pub interrupt: InterruptTransport,
    pub mem: GuestMemoryMmap,
    pub allow_idmap: bool,
    pub shm_region: Option<VirtioShmRegion>,
    pub passthrough_cfg: Option<passthrough::Config>,
    pub overlay_cfg: Option<OverlayConfig>,
    pub read_only: bool,
    pub virtual_entries: Vec<VirtualDirEntry>,
    pub stop_fd: EventFd,
    pub exit_code: Arc<AtomicI32>,
    pub restoring: bool,
    #[cfg(target_os = "macos")]
    pub map_sender: Option<Sender<WorkerMessage>>,
}

impl FsWorker {
    pub fn new(config: FsWorkerConfig) -> Result<Self, io::Error> {
        let FsWorkerConfig {
            fs_tag,
            queues,
            queue_evts,
            interrupt,
            mem,
            allow_idmap,
            shm_region,
            passthrough_cfg,
            overlay_cfg,
            read_only,
            virtual_entries,
            stop_fd,
            exit_code,
            restoring,
            #[cfg(target_os = "macos")]
            map_sender,
        } = config;
        let cache_setting = match std::env::var("PVISOR_VM_CACHE_EXPERIMENT") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "PVISOR_VM_CACHE_EXPERIMENT must be UTF-8",
                ));
            }
        };
        let root_data_cache = root_data_cache_enabled(&fs_tag, cache_setting.as_deref())?;
        let guest_cache =
            guest_cache_enabled(std::env::var_os("PVISOR_VM_GUEST_CACHE").as_deref())?;
        let host_writeback =
            host_writeback_enabled(std::env::var_os("PVISOR_VM_FS_HOST_WRITEBACK").as_deref())?;
        let host_writeback_config = host_writeback_config(
            std::env::var_os("PVISOR_VM_FS_HOST_WRITEBACK_CONFIG").as_deref(),
        )?;
        let directory_cache_capacity = directory_cache_capacity(
            std::env::var_os("PVISOR_VM_FS_DIRECTORY_CACHE_CAPACITY").as_deref(),
        )?;
        let exclusive_upper_absence_cache = exclusive_upper_absence_cache_enabled(
            std::env::var_os("PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE").as_deref(),
        )?;
        let readonly_flush_elision = readonly_flush_elision_enabled(
            std::env::var_os("PVISOR_VM_FS_READONLY_FLUSH_ELISION").as_deref(),
        )?;
        let lower_data_cache =
            lower_data_cache_enabled(std::env::var_os("PVISOR_VM_FS_LOWER_DATA_CACHE").as_deref())?;
        let negative_entry_cache = negative_entry_cache_enabled(
            std::env::var_os("PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE").as_deref(),
        )?;
        let inode_alloc = Arc::new(InodeAllocator::new());
        let mut server = match (overlay_cfg, passthrough_cfg) {
            (Some(cfg), _) => {
                let mut inner = if restoring {
                    OverlayFs::open_existing(cfg, inode_alloc.clone())?
                } else {
                    OverlayFs::new(cfg, inode_alloc.clone())?
                };
                inner.configure_host_writeback_options(
                    host_writeback,
                    shm_region.is_some(),
                    host_writeback_config.options()?,
                    host_writeback_config.qualification_lock,
                    host_writeback_config.synchronous,
                )?;
                inner.configure_immutable_data_cache(root_data_cache);
                inner.configure_directory_cache_capacity(directory_cache_capacity)?;
                inner.configure_exclusive_upper_absence_cache(
                    exclusive_upper_absence_cache,
                    guest_cache,
                )?;
                inner.configure_readonly_flush_elision(
                    readonly_flush_elision,
                    guest_cache,
                    shm_region.is_some(),
                )?;
                inner.configure_lower_data_cache(
                    lower_data_cache,
                    guest_cache,
                    shm_region.is_some(),
                )?;
                inner.configure_negative_entry_cache(
                    negative_entry_cache,
                    guest_cache,
                    shm_region.is_some(),
                    virtual_entries
                        .iter()
                        .map(|entry| {
                            std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
                                entry.name.to_bytes(),
                            ))
                        })
                        .collect(),
                )?;
                FsServer::Overlay(Box::new(Server::new(
                    AugmentFs::new(inner, &inode_alloc, virtual_entries),
                    guest_cache,
                )))
            }
            (None, Some(mut cfg)) if read_only => {
                // A shared-memory window on a read-only attachment is created
                // only by immutable_filesystem, whose caller owns physical
                // contents and namespace for this VM's lifetime. Ordinary ro
                // mounts do not imply that guarantee. Extend only metadata and
                // entry TTLs; keep the existing data-cache policy with dax=never.
                if shm_region.is_some() {
                    cfg.entry_timeout = std::time::Duration::from_secs(60);
                    // LinuxSimplified attributes depend on the requesting uid.
                    if !cfg.attr_timeout.is_zero() {
                        cfg.attr_timeout = std::time::Duration::from_secs(60);
                    }
                }
                let inner = PassthroughFsRo::new(cfg, inode_alloc.clone())?;
                #[cfg(target_os = "linux")]
                let inner = {
                    let mut inner = inner;
                    if let Some(region) = shm_region.as_ref() {
                        inner.enable_dax(region.clone())?;
                    }
                    inner
                };
                FsServer::ReadOnly(Server::new(
                    AugmentFs::new(inner, &inode_alloc, virtual_entries),
                    guest_cache,
                ))
            }
            (None, Some(cfg)) => {
                let inner = PassthroughFs::new(cfg, inode_alloc.clone())?;
                FsServer::ReadWrite(Server::new(
                    AugmentFs::new(inner, &inode_alloc, virtual_entries),
                    guest_cache,
                ))
            }
            (None, None) => FsServer::Null(Server::new(
                AugmentFs::new(NullFs, &inode_alloc, virtual_entries),
                guest_cache,
            )),
        };
        server.configure_profile_scope(&fs_tag);
        let dispatch_profile = server.dispatch_profile();
        Ok(Self {
            queues,
            queue_evts,
            interrupt,
            execution: Arc::new(Execution {
                mem,
                allow_idmap,
                shm_region,
                server,
                session: RwLock::new(()),
                exit_code,
                #[cfg(target_os = "macos")]
                map_sender,
            }),
            dispatch_profile,
            inode_alloc,
            stop_fd,
        })
    }

    pub fn capture_state(
        &self,
    ) -> io::Result<(
        Vec<super::super::QueueSnapshot>,
        super::snapshot::ServerSnapshot,
    )> {
        let next = self.inode_alloc.snapshot_next();
        let state = match &self.execution.server {
            FsServer::ReadWrite(s) => s.capture_state(next),
            FsServer::ReadOnly(s) => s.capture_state(next),
            FsServer::Null(s) => s.capture_state(next),
            FsServer::Overlay(s) => s.capture_state(next),
        }?;
        Ok((
            self.queues.iter().map(Queue::capture_state).collect(),
            state,
        ))
    }
    pub fn restore_state(&self, state: &super::snapshot::ServerSnapshot) -> io::Result<()> {
        if state.next_inode <= state.fs.max_inode()
            || state.next_inode < self.inode_alloc.snapshot_next()
            || state.next_inode == u64::MAX
        {
            return Err(super::snapshot::invalid(
                "invalid filesystem inode allocator",
            ));
        }
        match &self.execution.server {
            FsServer::ReadWrite(s) => s.restore_state(state),
            FsServer::ReadOnly(s) => s.restore_state(state),
            FsServer::Null(s) => s.restore_state(state),
            FsServer::Overlay(s) => s.restore_state(state),
        }?;
        self.inode_alloc.restore_next(state.next_inode)
    }

    pub fn run(self) -> thread::JoinHandle<Self> {
        thread::Builder::new()
            .name("fs worker".into())
            .spawn(|| self.work())
            .unwrap()
    }

    fn work(mut self) -> Self {
        self.queues = scheduler::serve(
            std::mem::take(&mut self.queues),
            &self.queue_evts,
            &self.stop_fd,
            self.interrupt.clone(),
            self.execution.clone(),
            self.dispatch_profile.clone(),
        );
        self.dispatch_profile.emit_checkpoint();
        self
    }

    #[cfg(test)]
    fn complete(&mut self, completion: Completion) {
        let index = completion.request.queue;
        self.publish_completion(completion);
        let mut completed = [false; 2];
        completed[index] = true;
        self.notify_completed(completed);
    }

    #[cfg(test)]
    fn publish_completion(&mut self, completion: Completion) {
        self.dispatch_profile
            .record_since("pool_completion_wait", completion.request.profile_timestamp);
        let queue = &mut self.queues[completion.request.queue];
        if let Err(error) = queue.add_used(
            &self.execution.mem,
            completion.request.index,
            completion.len as u32,
        ) {
            error!("failed to add filesystem used element: {error}");
        }
        // Keep the RAM lease through publication. Only already-ready pool
        // completions are batched; inline responses still notify immediately.
        self.dispatch_profile.add("published_completions", 1);
    }

    #[cfg(test)]
    fn notify_completed(&mut self, completed: [bool; 2]) {
        let mut notify = false;
        for (index, completed) in completed.into_iter().enumerate() {
            if completed {
                self.dispatch_profile.add("notification_checks", 1);
                notify |= self.queues[index]
                    .needs_notification(&self.execution.mem)
                    .unwrap();
            }
        }
        if notify {
            self.dispatch_profile.add("used_interrupts", 1);
            self.interrupt.signal_used_queue();
        }
    }

    #[cfg(test)]
    fn service_queues(&mut self, pool: &mut Option<RequestPool>, workers: usize, epoll: &Epoll) {
        let profile = self.dispatch_profile.clone();
        let _span = profile.span("admission");
        for index in [HPQ_INDEX, REQ_INDEX] {
            loop {
                self.queues[index]
                    .disable_notification(&self.execution.mem)
                    .unwrap();
                loop {
                    let Some(head) = self.queues[index].pop(&self.execution.mem) else {
                        break;
                    };
                    let head_index = head.index;
                    let buffers = match OwnedDescriptorChain::new(head) {
                        Ok(buffers) => buffers,
                        Err(error) => {
                            error!("invalid filesystem descriptor chain: {error}");
                            self.queues[index]
                                .add_used(&self.execution.mem, head_index, 0)
                                .unwrap();
                            if self.queues[index]
                                .needs_notification(&self.execution.mem)
                                .unwrap()
                            {
                                self.interrupt.signal_used_queue();
                            }
                            continue;
                        }
                    };
                    let decoded = buffers
                        .reader_writer(&self.execution.mem)
                        .map_err(super::FsError::QueueReader)
                        .and_then(|(mut reader, writer)| {
                            reader
                                .read_obj::<InHeader>()
                                .map(|header| (header, reader, writer))
                                .map_err(super::FsError::DecodeMessage)
                        });
                    let (header, reader, writer) = match decoded {
                        Ok(decoded) => decoded,
                        Err(error) => {
                            error!("invalid filesystem message: {error:?}");
                            self.complete(Completion {
                                request: Request {
                                    queue: index,
                                    index: head_index,
                                    header: InHeader::default(),
                                    buffers,
                                    profile_timestamp: None,
                                },
                                len: 0,
                            });
                            continue;
                        }
                    };
                    let parallel =
                        index == REQ_INDEX && workers > 1 && parallel_candidate(&header, &reader);
                    if parallel && pool.as_ref().is_some_and(|p| p.in_flight == p.limit) {
                        // Short inline requests may run even at pool capacity.
                        // Leave an expensive head in the ring rather than
                        // retaining another RAM lease or overflowing the pool.
                        self.queues[index].undo_pop();
                        self.dispatch_profile.add("pool_capacity_stalls", 1);
                        break;
                    }
                    let request = Request {
                        queue: index,
                        index: head_index,
                        header,
                        buffers,
                        profile_timestamp: None,
                    };
                    if parallel {
                        if pool.is_none() {
                            let execution = self.execution.clone();
                            match RequestPool::new(
                                move |request| execution.handle(request),
                                workers,
                                // Admit a complete bounded virtqueue, not just
                                // 2*workers: an I/O burst must not hide metadata
                                // behind the next long request in the avail ring.
                                usize::from(self.queues[REQ_INDEX].actual_size()),
                                self.dispatch_profile.clone(),
                            ) {
                                Ok(new_pool) => {
                                    let fd = new_pool.wake.as_raw_fd();
                                    epoll
                                        .ctl(
                                            ControlOperation::Add,
                                            fd,
                                            &EpollEvent::new(EventSet::IN, fd as u64),
                                        )
                                        .expect("filesystem completion registration");
                                    self.dispatch_profile.add("pool_workers", workers as u64);
                                    *pool = Some(new_pool);
                                }
                                Err(error) => {
                                    error!("filesystem pool unavailable; using inline I/O: {error}")
                                }
                            }
                        }
                        if let Some(pool) = pool {
                            self.dispatch_profile.add("pool_requests", 1);
                            pool.submit(request);
                            continue;
                        }
                    }
                    // Inline for short requests and hiprio FORGET/INTERRUPT.
                    // This retains the low-latency path and cannot queue behind
                    // an exhausted normal-request pool.
                    self.dispatch_profile.add("inline_requests", 1);
                    let len = self.execution.handle_decoded(header, reader, writer);
                    self.complete(Completion { request, len });
                }
                let pending = self.queues[index]
                    .enable_notification(&self.execution.mem)
                    .unwrap();
                if !pending
                    || (index == REQ_INDEX && pool.as_ref().is_some_and(|p| p.in_flight == p.limit))
                {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::virtio::descriptor_utils::{create_descriptor_chain, DescriptorType};
    use std::time::Duration;
    use vm_memory::GuestAddress;

    pub(super) fn queue_worker(mem: GuestMemoryMmap, queues: Vec<Queue>) -> FsWorker {
        use crate::devices::legacy::DummyIrqChip;
        let queue_count = queues.len();
        let mut worker = FsWorker::new(FsWorkerConfig {
            fs_tag: "test".into(),
            queues,
            queue_evts: (0..queue_count)
                .map(|_| Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()))
                .collect(),
            interrupt: InterruptTransport::new(DummyIrqChip::new().into(), "test".into()).unwrap(),
            mem,
            allow_idmap: false,
            shm_region: None,
            passthrough_cfg: None,
            overlay_cfg: None,
            read_only: false,
            virtual_entries: vec![],
            stop_fd: EventFd::new(EFD_NONBLOCK).unwrap(),
            exit_code: Arc::new(AtomicI32::new(0)),
            restoring: false,
            #[cfg(target_os = "macos")]
            map_sender: None,
        })
        .unwrap();
        worker.dispatch_profile =
            pvisor_overlay_core::profile::Profile::enabled("virtio-fs-dispatch");
        worker
    }

    #[test]
    fn host_writeback_config_is_bounded_and_rejects_unknown_fields() {
        use std::ffi::OsStr;
        let default = host_writeback_config(None).unwrap();
        assert_eq!(default.options().unwrap().budget_bytes, 64 << 20);
        assert!(!default.qualification_lock);
        let unmerged = host_writeback_config(Some(OsStr::new(
            r#"{"chunks":1,"delay_us":0,"capacity_mib":4,"batch_kib":4}"#,
        )))
        .unwrap();
        assert_eq!(unmerged.options().unwrap().max_chunks, 1);
        for value in [
            b"".as_slice(),
            b"null",
            b"[]",
            b"{\"unknown\":1}",
            b"{\"capacity_mib\":0}",
            b"{\"capacity_mib\":257}",
            b"{\"jobs\":0}",
            b"{\"chunks\":65}",
            b"{\"delay_us\":50001}",
            b"{\"batch_kib\":8192}",
            b"{\"jobs\":-1}",
            b"{\"qualification_lock\":1}",
            b"\xff",
        ] {
            assert_eq!(
                host_writeback_config(Some(OsStr::from_bytes(value)))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput,
                "{value:?}"
            );
        }
    }

    #[test]
    fn host_writeback_defaults_off_and_rejects_invalid_settings() {
        use std::ffi::OsStr;

        assert!(!host_writeback_enabled(None).unwrap());
        assert!(!host_writeback_enabled(Some(OsStr::new("0"))).unwrap());
        assert!(host_writeback_enabled(Some(OsStr::new("1"))).unwrap());
        for value in [
            b"".as_slice(),
            b"false",
            b"true",
            b"2",
            b"-1",
            b"01",
            b" 1",
            b"1 ",
            b"1\n",
            b"\xff",
        ] {
            let error = host_writeback_enabled(Some(OsStr::from_bytes(value))).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("PVISOR_VM_FS_HOST_WRITEBACK"));
        }
    }

    #[test]
    fn negative_entry_cache_defaults_off_and_rejects_invalid_settings() {
        use std::ffi::OsStr;
        assert!(!negative_entry_cache_enabled(None).unwrap());
        assert!(!negative_entry_cache_enabled(Some(OsStr::new("0"))).unwrap());
        assert!(negative_entry_cache_enabled(Some(OsStr::new("1"))).unwrap());
        for value in [b"".as_slice(), b"true", b"2", b" 1", b"1 ", b"\xff"] {
            assert_eq!(
                negative_entry_cache_enabled(Some(OsStr::from_bytes(value)))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn lower_data_cache_defaults_off_and_rejects_invalid_settings() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        assert!(!lower_data_cache_enabled(None).unwrap());
        assert!(!lower_data_cache_enabled(Some(OsStr::new("0"))).unwrap());
        assert!(lower_data_cache_enabled(Some(OsStr::new("1"))).unwrap());
        for value in [b"".as_slice(), b"true", b"2", b" 1", b"1 ", b"\xff"] {
            assert_eq!(
                lower_data_cache_enabled(Some(OsStr::from_bytes(value)))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn readonly_flush_elision_defaults_off_and_rejects_invalid_settings() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        assert!(!readonly_flush_elision_enabled(None).unwrap());
        assert!(!readonly_flush_elision_enabled(Some(OsStr::new("0"))).unwrap());
        assert!(readonly_flush_elision_enabled(Some(OsStr::new("1"))).unwrap());
        for invalid in ["", "false", "true", "2", " 1", "1 "] {
            assert_eq!(
                readonly_flush_elision_enabled(Some(OsStr::new(invalid)))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            readonly_flush_elision_enabled(Some(OsStr::from_bytes(b"\xff")))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn guest_cache_defaults_on_and_rejects_invalid_settings() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        assert!(guest_cache_enabled(None).unwrap());
        assert!(guest_cache_enabled(Some(OsStr::new("1"))).unwrap());
        assert!(!guest_cache_enabled(Some(OsStr::new("0"))).unwrap());
        for invalid in [
            OsStr::new(""),
            OsStr::new("off"),
            OsStr::from_bytes(b"\xff"),
        ] {
            assert_eq!(
                guest_cache_enabled(Some(invalid)).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
            );
        }
    }

    #[test]
    fn exclusive_upper_absence_experiment_defaults_off_and_rejects_invalid_settings() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        assert!(!exclusive_upper_absence_cache_enabled(None).unwrap());
        assert!(!exclusive_upper_absence_cache_enabled(Some(OsStr::new("0"))).unwrap());
        assert!(exclusive_upper_absence_cache_enabled(Some(OsStr::new("1"))).unwrap());
        for invalid in [
            OsStr::new(""),
            OsStr::new("off"),
            OsStr::new("true"),
            OsStr::new("2"),
            OsStr::new(" 1"),
            OsStr::new("1 "),
            OsStr::from_bytes(b"\xff"),
        ] {
            assert_eq!(
                exclusive_upper_absence_cache_enabled(Some(invalid))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput,
            );
        }
    }

    #[test]
    fn directory_reference_capacity_defaults_and_bounds_are_explicit() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        assert_eq!(directory_cache_capacity(None).unwrap(), 256);
        for capacity in [1, 256, 1024, 4096] {
            assert_eq!(
                directory_cache_capacity(Some(OsStr::new(&capacity.to_string()))).unwrap(),
                capacity
            );
        }
        for invalid in [
            OsStr::new(""),
            OsStr::new("0"),
            OsStr::new("4097"),
            OsStr::new("-1"),
            OsStr::new("+1024"),
            OsStr::new(" 1024"),
            OsStr::new("1024 "),
            OsStr::new("1.5"),
            OsStr::new("184467440737095516160"),
            OsStr::from_bytes(b"\xff"),
        ] {
            assert_eq!(
                directory_cache_capacity(Some(invalid)).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
            );
        }
    }

    #[test]
    fn root_data_cache_defaults_to_root_only_and_has_an_off_control() {
        for setting in [None, Some("root-data")] {
            assert!(root_data_cache_enabled("/dev/root", setting).unwrap());
            assert!(!root_data_cache_enabled("pvisor-workspace", setting).unwrap());
            assert!(!root_data_cache_enabled("tools", setting).unwrap());
        }
        assert!(!root_data_cache_enabled("/dev/root", Some("off")).unwrap());
        for unsupported in ["", "combined", "root-meta", "work-meta", "ro-noopen"] {
            assert_eq!(
                root_data_cache_enabled("/dev/root", Some(unsupported))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput,
            );
        }
    }

    fn check_metadata_admission(opcodes: [Opcode; 2], limit: usize, pooled: usize, remaining: u16) {
        use crate::devices::virtio::queue::tests::VirtQueue;
        use vm_memory::Bytes;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let normal = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        for (slot, opcode) in opcodes.into_iter().enumerate().map(|(i, op)| (i * 2, op)) {
            let address = 0x4000 + slot as u64 * 0x200;
            let len = if matches!(opcode, Opcode::Read) {
                std::mem::size_of::<InHeader>() + std::mem::size_of::<ReadIn>()
            } else {
                64
            } as u32;
            normal.dtable[slot].addr.set(address);
            normal.dtable[slot].len.set(len);
            normal.dtable[slot]
                .flags
                .set(crate::devices::virtio::queue::VIRTQ_DESC_F_NEXT);
            normal.dtable[slot].next.set((slot + 1) as u16);
            normal.dtable[slot + 1].addr.set(address + 0x100);
            normal.dtable[slot + 1].len.set(256);
            normal.dtable[slot + 1]
                .flags
                .set(crate::devices::virtio::queue::VIRTQ_DESC_F_WRITE);
            mem.write_obj(
                InHeader {
                    len,
                    opcode: opcode as u32,
                    nodeid: 1,
                    ..Default::default()
                },
                GuestAddress(address),
            )
            .unwrap();
            if matches!(opcode, Opcode::Read) {
                mem.write_obj(
                    ReadIn {
                        size: 64 * 1024,
                        ..Default::default()
                    },
                    GuestAddress(address + std::mem::size_of::<InHeader>() as u64),
                )
                .unwrap();
            }
        }
        normal.avail.ring[0].set(0);
        normal.avail.ring[1].set(2);
        normal.avail.idx.set(2);
        let mut worker = queue_worker(
            mem.clone(),
            vec![high.create_queue(), normal.create_queue()],
        );
        let (resume, waiting) = crossbeam_channel::unbounded();
        let mut full = RequestPool::new(
            move |_| {
                waiting.recv().unwrap();
                0
            },
            2,
            limit,
            Default::default(),
        )
        .unwrap();
        // Fill the old 2*workers budget, without releasing either I/O worker.
        for index in 0..4 {
            let chain = create_descriptor_chain(
                &mem,
                GuestAddress(0x1000 + index as u64 * 0x100),
                GuestAddress(0x2000 + index as u64 * 0x100),
                vec![(DescriptorType::Readable, 8), (DescriptorType::Writable, 8)],
                0,
            )
            .unwrap();
            full.submit(Request {
                queue: REQ_INDEX,
                index: index as u16,
                header: InHeader::default(),
                buffers: OwnedDescriptorChain::new(chain).unwrap(),
                profile_timestamp: None,
            });
        }
        let mut pool = Some(full);
        worker.service_queues(&mut pool, 2, &Epoll::new().unwrap());
        let in_flight = pool.as_ref().unwrap().in_flight;
        for _ in 0..in_flight {
            resume.send(()).unwrap();
        }
        let report = worker.dispatch_profile.report().unwrap();
        drop(pool);
        assert_eq!(normal.used.idx.get(), 1);
        assert_eq!(worker.queues[REQ_INDEX].len(&mem), remaining);
        assert_eq!(report.measurements["inline_requests"].units, 1);
        assert_eq!(in_flight, 4 + pooled);
        if pooled == 0 {
            assert_eq!(report.measurements["pool_capacity_stalls"].units, 1);
        } else {
            assert_eq!(report.measurements["pool_requests"].units, pooled as u64);
            assert!(!report.measurements.contains_key("pool_capacity_stalls"));
        }
    }

    #[test]
    fn full_pool_admits_inline_metadata_but_leaves_expensive_head_in_ring() {
        check_metadata_admission([Opcode::Getattr, Opcode::Opendir], 4, 0, 1);
    }

    #[test]
    fn metadata_behind_a_long_request_bypasses_busy_io_workers() {
        check_metadata_admission([Opcode::Opendir, Opcode::Getattr], 8, 1, 0);
    }

    #[test]
    fn metadata_behind_a_large_read_bypasses_busy_io_workers() {
        check_metadata_admission([Opcode::Read, Opcode::Getattr], 8, 1, 0);
    }

    #[test]
    fn lone_directory_request_is_offloaded_and_published_by_the_owner() {
        use crate::devices::virtio::queue::tests::VirtQueue;
        use vm_memory::Bytes;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let normal = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        normal.dtable[0].addr.set(0x4000);
        normal.dtable[0].len.set(64);
        normal.dtable[0]
            .flags
            .set(crate::devices::virtio::queue::VIRTQ_DESC_F_NEXT);
        normal.dtable[0].next.set(1);
        normal.dtable[1].addr.set(0x4100);
        normal.dtable[1].len.set(256);
        normal.dtable[1]
            .flags
            .set(crate::devices::virtio::queue::VIRTQ_DESC_F_WRITE);
        mem.write_obj(
            InHeader {
                len: 64,
                opcode: Opcode::Opendir as u32,
                nodeid: 1,
                ..Default::default()
            },
            GuestAddress(0x4000),
        )
        .unwrap();
        normal.avail.ring[0].set(0);
        normal.avail.idx.set(1);
        let mut worker = queue_worker(
            mem.clone(),
            vec![high.create_queue(), normal.create_queue()],
        );
        let mut pool = None;
        worker.service_queues(&mut pool, 2, &Epoll::new().unwrap());
        assert_eq!(normal.used.idx.get(), 0);
        assert_eq!(worker.queues[REQ_INDEX].len(&mem), 0);
        let pool = pool
            .as_mut()
            .expect("lone directory request must use I/O pool");
        assert_eq!(pool.limit, 8);
        assert_eq!(pool.in_flight, 1);
        let completion = pool.completed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(completion.len >= std::mem::size_of::<super::super::fuse::OutHeader>());
        worker.complete(completion);
        pool.in_flight -= 1;
        assert_eq!(normal.used.idx.get(), 1);
        let report = worker.dispatch_profile.report().unwrap();
        assert_eq!(report.measurements["pool_requests"].units, 1);
        assert!(!report.measurements.contains_key("inline_requests"));
    }

    #[test]
    fn ready_completions_publish_before_a_single_event_idx_notification_check() {
        use crate::devices::virtio::queue::tests::VirtQueue;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let normal = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        let mut queue = normal.create_queue();
        queue.set_event_idx(true);
        let mut worker = queue_worker(mem.clone(), vec![high.create_queue(), queue]);
        for index in 0..2 {
            let chain = create_descriptor_chain(
                &mem,
                GuestAddress(0x1000 + index as u64 * 0x100),
                GuestAddress(0x2000 + index as u64 * 0x100),
                vec![(DescriptorType::Readable, 8), (DescriptorType::Writable, 8)],
                0,
            )
            .unwrap();
            worker.publish_completion(Completion {
                request: Request {
                    queue: REQ_INDEX,
                    index,
                    header: InHeader::default(),
                    buffers: OwnedDescriptorChain::new(chain).unwrap(),
                    profile_timestamp: None,
                },
                len: 8,
            });
        }
        assert_eq!(normal.used.idx.get(), 2);
        assert!(!worker
            .dispatch_profile
            .report()
            .unwrap()
            .measurements
            .contains_key("notification_checks"));
        worker.notify_completed([false, true]);
        let report = worker.dispatch_profile.report().unwrap();
        assert_eq!(report.measurements["published_completions"].units, 2);
        assert_eq!(report.measurements["notification_checks"].units, 1);
        assert_eq!(report.measurements["used_interrupts"].units, 1);
    }

    #[test]
    fn pool_eligibility_keeps_short_metadata_and_all_opens_inline() {
        use super::super::fuse::OpenIn;
        use vm_memory::Bytes;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![
                (
                    DescriptorType::Readable,
                    std::mem::size_of::<OpenIn>() as u32,
                ),
                (DescriptorType::Writable, 256),
            ],
            0,
        )
        .unwrap();
        let buffers = OwnedDescriptorChain::new(chain).unwrap();
        let (reader, _) = buffers.reader_writer(&mem).unwrap();
        let header = InHeader {
            opcode: Opcode::Open as u32,
            ..Default::default()
        };
        for (flags, open_flags, expected) in [
            (libc::O_RDONLY, 0, false),
            (libc::O_WRONLY, 0, false),
            (libc::O_RDWR, 0, false),
            (libc::O_RDONLY | libc::O_TRUNC, 0, false),
            (libc::O_RDONLY | libc::O_APPEND, 0, false),
            (libc::O_RDONLY, super::super::fuse::OPEN_KILL_SUIDGID, false),
        ] {
            mem.write_obj(
                OpenIn {
                    flags: flags as u32,
                    open_flags,
                },
                GuestAddress(0x100),
            )
            .unwrap();
            assert_eq!(
                parallel_candidate(&header, &reader),
                expected,
                "flags={flags} open_flags={open_flags}"
            );
        }
        for opcode in [Opcode::Lookup, Opcode::Getattr] {
            assert!(!parallel_candidate(
                &InHeader {
                    opcode: opcode as u32,
                    ..Default::default()
                },
                &reader
            ));
        }
        // A short body must not be classified as a large read.
        assert!(!parallel_candidate(
            &InHeader {
                opcode: Opcode::Read as u32,
                ..Default::default()
            },
            &reader
        ));
    }

    #[test]
    fn guest_header_changes_do_not_replace_an_admitted_request() {
        use vm_memory::{ByteValued, Bytes};
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let header = InHeader {
            len: (std::mem::size_of::<InHeader>()
                + std::mem::size_of::<super::super::fuse::GetattrIn>()) as u32,
            opcode: Opcode::Getattr as u32,
            nodeid: 1,
            unique: 99,
            ..Default::default()
        };
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![
                (DescriptorType::Readable, header.len),
                (DescriptorType::Writable, 256),
            ],
            0,
        )
        .unwrap();
        mem.write_slice(header.as_slice(), GuestAddress(0x100))
            .unwrap();
        let request = Request {
            queue: REQ_INDEX,
            index: 0,
            header,
            buffers: OwnedDescriptorChain::new(chain).unwrap(),
            profile_timestamp: None,
        };
        mem.write_obj(
            InHeader {
                opcode: Opcode::Destroy as u32,
                ..Default::default()
            },
            GuestAddress(0x100),
        )
        .unwrap();
        let allocator = InodeAllocator::new();
        let execution = Execution {
            mem: mem.clone(),
            server: FsServer::Null(Server::new(
                AugmentFs::new(NullFs, &allocator, vec![]),
                true,
            )),
            session: RwLock::new(()),
            allow_idmap: false,
            shm_region: None,
            exit_code: Arc::new(AtomicI32::new(0)),
            #[cfg(target_os = "macos")]
            map_sender: None,
        };
        let len = execution.handle(&request);
        assert!(len > std::mem::size_of::<super::super::fuse::OutHeader>());
        let reply: super::super::fuse::OutHeader = mem
            .read_obj(GuestAddress(0x100 + u64::from(header.len)))
            .unwrap();
        assert_eq!(reply.error, 0);
        assert_eq!(reply.unique, 99);
    }

    #[test]
    fn completion_wakes_only_an_armed_owner_and_ready_completion_never_parks() {
        for armed in [false, true] {
            let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
            let chain = create_descriptor_chain(
                &mem,
                GuestAddress(0),
                GuestAddress(0x2000),
                vec![(DescriptorType::Readable, 8), (DescriptorType::Writable, 8)],
                0,
            )
            .unwrap();
            let mut pool = RequestPool::new(|_| 8, 2, 8, Default::default()).unwrap();
            if armed {
                assert_eq!(pool.wait_timeout(), -1);
            }
            pool.submit(Request {
                queue: REQ_INDEX,
                index: 0,
                header: InHeader::default(),
                buffers: OwnedDescriptorChain::new(chain).unwrap(),
                profile_timestamp: None,
            });
            let deadline = Instant::now() + std::time::Duration::from_secs(2);
            while pool.completed.is_empty() {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            let wake = pool.wake.clone();
            // Joining ensures the worker finished the send/disarm/wake sequence.
            for worker in pool.threads.drain(..) {
                // Disconnect both job inputs before joining.
                pool.jobs.take();
                pool.idle_jobs.take();
                worker.join().unwrap();
            }
            assert_eq!(wake.read().is_ok(), armed);
            assert_eq!(pool.wait_timeout(), 0);
            assert_eq!(pool.completed.try_recv().unwrap().len, 8);
            pool.in_flight -= 1;
        }
    }

    #[test]
    fn isolated_requests_reuse_the_primary_worker_without_losing_completion() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let caller = thread::current().id();
        let mut pool = RequestPool::new(
            move |_| {
                assert_ne!(thread::current().id(), caller);
                assert_eq!(thread::current().name(), Some("fs io 0"));
                8
            },
            4,
            8,
            Default::default(),
        )
        .unwrap();
        for index in 0..16 {
            let chain = create_descriptor_chain(
                &mem,
                GuestAddress(index as u64 * 0x100),
                GuestAddress(0x2000 + index as u64 * 0x100),
                vec![(DescriptorType::Readable, 8), (DescriptorType::Writable, 8)],
                0,
            )
            .unwrap();
            pool.submit(Request {
                queue: REQ_INDEX,
                index,
                header: InHeader::default(),
                buffers: OwnedDescriptorChain::new(chain).unwrap(),
                profile_timestamp: None,
            });
            let completed = pool
                .completed
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert_eq!(completed.request.index, index);
            assert_eq!(completed.len, 8);
            pool.in_flight -= 1;
        }
        drop(pool);
    }

    #[test]
    fn pool_completes_out_of_order_and_retains_unpublished_memory_leases() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let gate = crate::devices::virtio::memory_gate::register(&mem);
        let request = |index| {
            let chain = create_descriptor_chain(
                &mem,
                GuestAddress(index as u64 * 0x100),
                GuestAddress(0x1000 + index as u64 * 0x100),
                vec![(DescriptorType::Readable, 8), (DescriptorType::Writable, 8)],
                0,
            )
            .unwrap();
            Request {
                queue: REQ_INDEX,
                index,
                header: InHeader::default(),
                buffers: OwnedDescriptorChain::new(chain).unwrap(),
                profile_timestamp: None,
            }
        };
        let (started, waiting) = crossbeam_channel::bounded(1);
        let (resume, released) = crossbeam_channel::bounded(1);
        let mut pool = RequestPool::new(
            move |request| {
                if request.index == 0 {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                }
                8
            },
            2,
            4,
            pvisor_overlay_core::profile::Profile::default(),
        )
        .unwrap();
        assert_eq!(pool.limit, 4);
        pool.submit(request(0));
        pool.submit(request(1));
        let started = waiting.recv_timeout(Duration::from_secs(2));
        let fast = pool.completed.recv_timeout(Duration::from_secs(2));
        let retained = gate.try_close();
        // Unblock before every fallible assertion so a regression cannot hang
        // RequestPool::drop while it joins the deliberately blocked worker.
        resume.send(()).unwrap();
        let slow = pool.completed.recv_timeout(Duration::from_secs(2));
        pool.in_flight -= 2;
        drop(pool); // shutdown joins every executing thread
        started.unwrap();
        let fast = fast.unwrap();
        let slow = slow.unwrap();
        assert_eq!(fast.request.index, 1);
        assert_eq!(fast.len, 8);
        assert!(!retained.unwrap());
        assert!(!gate.try_close().unwrap());
        drop((fast, slow));
        assert!(gate.try_close().unwrap());
    }
}
