#[cfg(target_os = "macos")]
use crate::utils::worker_message::WorkerMessage;
#[cfg(target_os = "macos")]
use crossbeam_channel::Sender;

use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Instant;

use crate::utils::epoll::{ControlOperation, Epoll, EpollEvent, EventSet};
use crate::utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::GuestMemoryMmap;

use super::super::Queue;
use super::augment_fs::AugmentFs;
use super::defs::{HPQ_INDEX, REQ_INDEX};
use super::descriptor_utils::{OwnedDescriptorChain, Reader, Writer};
use super::fuse::{InHeader, Opcode, ReadIn};
use super::inode_alloc::InodeAllocator;
use super::null_fs::NullFs;
use super::overlay::{Config as OverlayConfig, OverlayFs};
use super::passthrough::{self, PassthroughFs};
use super::read_only::PassthroughFsRo;
use super::server::Server;
use super::virtual_entry::VirtualDirEntry;
use crate::devices::virtio::{InterruptTransport, VirtioShmRegion};

enum FsServer {
    ReadWrite(Server<AugmentFs<PassthroughFs>>),
    ReadOnly(Server<AugmentFs<PassthroughFsRo>>),
    Null(Server<AugmentFs<NullFs>>),
    Overlay(Box<Server<AugmentFs<OverlayFs>>>),
}

impl FsServer {
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
            .reader_writer(&self.mem)
            .and_then(|(mut reader, writer)| {
                // The owner captured the header exactly once. Skip its guest bytes
                // without trusting a second read of the driver-controlled opcode.
                reader
                    .split_at(std::mem::size_of::<InHeader>())
                    .map(|body| (body, writer))
            });
        match decoded {
            Ok((reader, writer)) => self.handle_decoded(request.header, reader, writer),
            Err(error) => {
                error!("invalid filesystem buffers: {error}");
                0
            }
        }
    }

    fn handle_decoded(&self, header: InHeader, reader: Reader, writer: Writer) -> usize {
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
        let result =
            if header.opcode == Opcode::Init as u32 || header.opcode == Opcode::Destroy as u32 {
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

/// Keep short metadata operations inline to avoid a thread handoff. Directory
/// enumeration and large reads can use the pool when multiple requests exist.
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
    // Submit timestamp, replaced by completion-ready timestamp after service.
    // None when diagnostics are disabled; normal requests read no clock.
    profile_timestamp: Option<Instant>,
}

struct Completion {
    // Keep both captured descriptors and the RAM lease until add_used finishes.
    request: Request,
    len: usize,
}

/// A per-device bounded blocking-I/O pool. Completion is asynchronous to the
/// queue owner; the existing pread/pwrite implementation remains zero-copy.
/// No borrowed VolatileSlice or unsafe Send implementation crosses threads.
struct RequestPool {
    jobs: Option<crossbeam_channel::Sender<Request>>,
    completed: crossbeam_channel::Receiver<Completion>,
    wake: Arc<EventFd>,
    threads: Vec<thread::JoinHandle<()>>,
    in_flight: usize,
    limit: usize,
    profile: pvisor_overlay_core::profile::Profile,
}

impl RequestPool {
    fn new(
        handle: impl Fn(&Request) -> usize + Send + Sync + 'static,
        workers: usize,
        profile: pvisor_overlay_core::profile::Profile,
    ) -> io::Result<Self> {
        let handle = Arc::new(handle);
        let limit = workers * 2;
        let (jobs, receive) = crossbeam_channel::bounded::<Request>(limit);
        // Although this channel is unbounded, at most `limit` accepted requests
        // can exist, including queued, executing and unpublished completions.
        let (completed, completions) = crossbeam_channel::unbounded();
        let wake = Arc::new(EventFd::new(EFD_NONBLOCK)?);
        let mut pool = Self {
            jobs: Some(jobs),
            completed: completions,
            wake,
            threads: Vec::new(),
            in_flight: 0,
            limit,
            profile,
        };
        for id in 0..workers {
            let receive = receive.clone();
            let completed = completed.clone();
            let wake = pool.wake.clone();
            let handle = handle.clone();
            let profile = pool.profile.clone();
            pool.threads
                .push(
                    thread::Builder::new()
                        .name(format!("fs io {id}"))
                        .spawn(move || {
                            for mut request in receive {
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
                                if let Err(error) = wake.write(1) {
                                    error!("filesystem completion wake failed: {error}");
                                    std::process::abort();
                                }
                            }
                        })?,
                );
        }
        Ok(pool)
    }

    fn submit(&mut self, mut request: Request) {
        request.profile_timestamp = self.profile.timestamp();
        assert!(self.in_flight < self.limit);
        // Capacity covers every in-flight request, so the owner never blocks
        // behind I/O and can always service the dedicated high-priority queue.
        self.jobs
            .as_ref()
            .expect("filesystem pool closed")
            .try_send(request)
            .unwrap_or_else(|_| panic!("filesystem pool capacity invariant violated"));
        self.in_flight += 1;
    }
}

impl Drop for RequestPool {
    fn drop(&mut self) {
        self.jobs.take();
        for worker in self.threads.drain(..) {
            if worker.join().is_err() {
                std::process::abort();
            }
        }
    }
}

fn worker_count() -> usize {
    // Host-side diagnostic override. The public VM API and guest queue topology
    // stay platform-independent. One restores entirely synchronous dispatch.
    std::env::var("PVISOR_VM_FS_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| (1..=8).contains(count))
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, |n| n.get().min(4)))
}

pub(super) struct FsWorkerConfig {
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
        let inode_alloc = Arc::new(InodeAllocator::new());
        let server = match (overlay_cfg, passthrough_cfg) {
            (Some(cfg), _) => {
                let inner = if restoring {
                    OverlayFs::open_existing(cfg, inode_alloc.clone())?
                } else {
                    OverlayFs::new(cfg, inode_alloc.clone())?
                };
                FsServer::Overlay(Box::new(Server::new(AugmentFs::new(
                    inner,
                    &inode_alloc,
                    virtual_entries,
                ))))
            }
            (None, Some(cfg)) if read_only => {
                let inner = PassthroughFsRo::new(cfg, inode_alloc.clone())?;
                FsServer::ReadOnly(Server::new(AugmentFs::new(
                    inner,
                    &inode_alloc,
                    virtual_entries,
                )))
            }
            (None, Some(cfg)) => {
                let inner = PassthroughFs::new(cfg, inode_alloc.clone())?;
                FsServer::ReadWrite(Server::new(AugmentFs::new(
                    inner,
                    &inode_alloc,
                    virtual_entries,
                )))
            }
            (None, None) => FsServer::Null(Server::new(AugmentFs::new(
                NullFs,
                &inode_alloc,
                virtual_entries,
            ))),
        };
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
            dispatch_profile: pvisor_overlay_core::profile::Profile::from_env("virtio-fs-dispatch"),
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
        let workers = worker_count();
        let mut pool = None;
        let epoll = Epoll::new().unwrap();
        for fd in self
            .queue_evts
            .iter()
            .map(|event| event.as_raw_fd())
            .chain(std::iter::once(self.stop_fd.as_raw_fd()))
        {
            epoll
                .ctl(
                    ControlOperation::Add,
                    fd,
                    &EpollEvent::new(EventSet::IN, fd as u64),
                )
                .expect("filesystem epoll registration");
        }
        // A restored/thawed queue may already contain requests without a new
        // kick. Scan before waiting, and after every completion frees capacity.
        self.service_queues(&mut pool, workers, &epoll);
        let mut events = vec![EpollEvent::new(EventSet::empty(), 0); 32];
        loop {
            let count = match epoll.wait(events.len(), -1, &mut events) {
                Ok(count) => count,
                Err(error) => {
                    debug!("filesystem epoll wait: {error}");
                    continue;
                }
            };
            // Stop admission before servicing any other event in this batch.
            if events[..count]
                .iter()
                .any(|e| e.fd() == self.stop_fd.as_raw_fd())
            {
                let _ = self.stop_fd.read();
                if let Some(pool) = &mut pool {
                    let mut completed = [false; 2];
                    while pool.in_flight != 0 {
                        let completion = pool.completed.recv().expect("filesystem completion lost");
                        completed[completion.request.queue] = true;
                        self.publish_completion(completion);
                        pool.in_flight -= 1;
                    }
                    self.notify_completed(completed);
                }
                // Dropping the pool joins every I/O worker before returning the
                // parked device to capture_state, reset or offload.
                drop(pool);
                self.dispatch_profile.emit_checkpoint();
                return self;
            }
            for event in &events[..count] {
                if let Some(queue) = self.queue_evts.iter().find(|q| q.as_raw_fd() == event.fd()) {
                    let _ = queue.read();
                } else if let Some(pool) = &mut pool {
                    if event.fd() == pool.wake.as_raw_fd() {
                        // macOS uses a pipe, so consume all pending wake tokens.
                        while pool.wake.read().is_ok() {}
                    }
                }
            }
            if let Some(pool) = &mut pool {
                let mut completed = [false; 2];
                while let Ok(completion) = pool.completed.try_recv() {
                    completed[completion.request.queue] = true;
                    self.publish_completion(completion);
                    pool.in_flight -= 1;
                }
                self.notify_completed(completed);
            }
            self.service_queues(&mut pool, workers, &epoll);
        }
    }

    fn complete(&mut self, completion: Completion) {
        let index = completion.request.queue;
        self.publish_completion(completion);
        let mut completed = [false; 2];
        completed[index] = true;
        self.notify_completed(completed);
    }

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
                    let parallel = index == REQ_INDEX
                        && workers > 1
                        && parallel_candidate(&header, &reader)
                        && (pool.as_ref().is_some_and(|p| p.in_flight != 0)
                            || !self.queues[index].is_empty(&self.execution.mem));
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
                    // Inline for a lone request, and for hiprio FORGET/INTERRUPT.
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

    fn queue_worker(mem: GuestMemoryMmap, queues: Vec<Queue>) -> FsWorker {
        use crate::devices::legacy::DummyIrqChip;
        let mut worker = FsWorker::new(FsWorkerConfig {
            queues,
            queue_evts: (0..2)
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
    fn full_pool_admits_inline_metadata_but_leaves_expensive_head_in_ring() {
        use crate::devices::virtio::queue::tests::VirtQueue;
        use vm_memory::Bytes;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let normal = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        for (slot, opcode) in [(0, Opcode::Getattr), (2, Opcode::Opendir)] {
            let address = 0x4000 + slot as u64 * 0x200;
            normal.dtable[slot].addr.set(address);
            normal.dtable[slot].len.set(64);
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
                    len: 64,
                    opcode: opcode as u32,
                    nodeid: 1,
                    ..Default::default()
                },
                GuestAddress(address),
            )
            .unwrap();
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
            Default::default(),
        )
        .unwrap();
        for index in 0..full.limit {
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
        for _ in 0..4 {
            resume.send(()).unwrap();
        }
        let report = worker.dispatch_profile.report().unwrap();
        drop(pool);
        assert_eq!(normal.used.idx.get(), 1);
        assert_eq!(worker.queues[REQ_INDEX].len(&mem), 1);
        assert_eq!(report.measurements["inline_requests"].units, 1);
        assert_eq!(report.measurements["pool_capacity_stalls"].units, 1);
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
            server: FsServer::Null(Server::new(AugmentFs::new(NullFs, &allocator, vec![]))),
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
